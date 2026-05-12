use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc, watch};
use tracing::{info, warn};

use crate::config::Config;
use crate::daemon::ipc::{self, DaemonState, IpcCommand};
use crate::daemon::model::WhisperModel;
use crate::daemon::shutdown::setup_shutdown_signal;
use crate::daemon::state_machine::{run_inference, start_recording, stop_recording};
use crate::output::injector::TextInjector;

/// Inicia e executa o daemon completo.
/// Bloqueia até receber SIGTERM ou erro fatal.
pub async fn run(config: Config) -> anyhow::Result<()> {
    info!("Iniciando whisper-dictate daemon");

    // --- Carrega o modelo na GPU (operação mais custosa — feita uma única vez) ---
    info!("Carregando modelo: {}", config.model.path);
    let model = Arc::new(
        WhisperModel::load(&config.model)
            .map_err(|e| anyhow::anyhow!("Falha ao carregar modelo Whisper: {}", e))?,
    );
    info!("Modelo carregado na VRAM com sucesso.");

    // --- Canais de comunicação entre tasks ---

    // Comandos vindos do IPC para o loop principal
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IpcCommand>(8);

    // Estado atual do daemon (watch = múltiplos leitores, sempre têm o valor mais recente)
    let (state_tx, state_rx) = watch::channel(DaemonState::Idle);

    // Canal para receber o áudio capturado de volta da task de gravação
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(1);

    // --- Injector de teclado virtual Wayland ---
    let injector = Arc::new(Mutex::new(TextInjector::new()?));

    // --- Resolve e inicia o servidor IPC ---
    let socket_path: PathBuf = config.ipc.resolved_socket_path()?;
    ipc::start_server(socket_path.clone(), cmd_tx, state_rx.clone()).await?;

    // --- Handle de SIGTERM para shutdown limpo ---
    let shutdown = setup_shutdown_signal();

    // --- Loop principal da máquina de estados ---
    let mut capture_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut notification_handle: Option<notify_rust::NotificationHandle> = None;

    info!(
        "Daemon pronto. Aguardando comandos via: {}",
        socket_path.display()
    );

    tokio::select! {
        _ = shutdown => {
            info!("SIGTERM recebido, encerrando daemon.");
        }

        _ = main_loop(
            &config,
            &model,
            &injector,
            &state_tx,
            &mut cmd_rx,
            &mut audio_rx,
            &audio_tx,
            &mut capture_handle,
            &mut notification_handle,
        ) => {}
    }

    // Cleanup: remove o socket ao encerrar
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
        info!("Socket IPC removido.");
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn main_loop(
    config: &Config,
    model: &Arc<WhisperModel>,
    injector: &Arc<Mutex<TextInjector>>,
    state_tx: &watch::Sender<DaemonState>,
    cmd_rx: &mut mpsc::Receiver<IpcCommand>,
    audio_rx: &mut mpsc::Receiver<Vec<f32>>,
    audio_tx: &mpsc::Sender<Vec<f32>>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) -> anyhow::Result<()> {
    let mut discard_next_audio = false;

    loop {
        tokio::select! {
            // --- Comando recebido via IPC ---
            Some(cmd) = cmd_rx.recv() => {
                let current_state = *state_tx.borrow();

                match (cmd, current_state) {
                    // Toggle em Idle → inicia gravação
                    (IpcCommand::Toggle, DaemonState::Idle) => {
                        discard_next_audio = false;
                        start_recording(
                            config,
                            audio_tx.clone(),
                            state_tx,
                            capture_handle,
                            notification_handle,
                        ).await?;
                    }

                    // Toggle ou Stop em Recording → encerra gravação, aguarda áudio
                    (IpcCommand::Toggle | IpcCommand::Stop, DaemonState::Recording) => {
                        stop_recording(
                            state_tx,
                            capture_handle,
                            notification_handle,
                        ).await;
                        // O áudio chegará pelo canal audio_rx; será processado abaixo
                    }

                    // Toggle em Processing → ignora (aguarda inferência terminar)
                    (IpcCommand::Toggle, DaemonState::Processing) => {
                        info!("Toggle ignorado: inferência em andamento.");
                    }

                    // Stop em Processing: descarta áudio pendente (se ainda não iniciou inferência)
                    (IpcCommand::Stop, DaemonState::Processing) => {
                        info!("Stop durante processamento: descartando áudio pendente.");
                        discard_next_audio = true;
                        state_tx.send(DaemonState::Idle)?;
                    }

                    _ => {}
                }
            }

            // --- Áudio completo recebido da task de captura ---
            Some(audio_buffer) = audio_rx.recv() => {
                if discard_next_audio {
                    info!("Áudio pendente descartado após stop durante processamento.");
                    discard_next_audio = false;
                    let _ = state_tx.send(DaemonState::Idle);
                    continue;
                }

                if *state_tx.borrow() != DaemonState::Processing {
                    warn!("Áudio recebido fora do estado Processing; buffer descartado.");
                    continue;
                }

                // Transição para Processing já foi feita em stop_recording
                run_inference(
                    config,
                    model,
                    injector,
                    state_tx,
                    audio_buffer,
                ).await?;
            }
        }
    }
}
