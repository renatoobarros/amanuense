use std::path::PathBuf;
use std::sync::{Arc, Mutex}; // Usando Mutex do std::sync

use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::config::Config;
use crate::daemon::ipc::{self, DaemonState, IpcCommand};
use crate::daemon::model::WhisperModel;
use crate::daemon::shutdown::setup_shutdown_signal;
use crate::daemon::state_machine::{run_inference, start_recording, stop_recording};
use crate::output::injector::TextInjector;

pub async fn run(config: Config) -> anyhow::Result<()> {
    info!("Iniciando amanuense daemon");

    info!("Carregando modelo: {}", config.model.path);
    let model = Arc::new(
        WhisperModel::load(&config.model)
            .map_err(|e| anyhow::anyhow!("Falha ao carregar modelo Whisper: {}", e))?,
    );
    info!("Modelo carregado na VRAM com sucesso.");

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IpcCommand>(8);
    let (state_tx, state_rx) = watch::channel(DaemonState::Idle);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(1);

    // Mutex síncrono padrão do Rust, não o do Tokio
    let injector = Arc::new(Mutex::new(TextInjector::new()?));

    let socket_path: PathBuf = config.ipc.resolved_socket_path()?;
    ipc::start_server(socket_path.clone(), cmd_tx, state_rx.clone()).await?;

    let shutdown = setup_shutdown_signal();
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

    if socket_path.exists() {
        match std::fs::remove_file(&socket_path) {
            Ok(_) => info!("Socket IPC removido."),
            Err(e) => warn!("Falha ao remover socket IPC: {}", e),
        }
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
            Some(cmd) = cmd_rx.recv() => {
                let current_state = *state_tx.borrow();

                match (cmd, current_state) {
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

                    (IpcCommand::Toggle | IpcCommand::Stop, DaemonState::Recording) => {
                        stop_recording(
                            state_tx,
                            capture_handle,
                            notification_handle,
                        ).await;
                    }

                    (IpcCommand::Toggle, DaemonState::Processing) => {
                        info!("Toggle ignorado: inferência em andamento.");
                    }

                    (IpcCommand::Stop, DaemonState::Processing) => {
                        info!("Stop durante processamento: descartando áudio pendente.");
                        discard_next_audio = true;
                        let _ = state_tx.send(DaemonState::Idle);
                    }
                    _ => {}
                }
            }

            Some(audio_buffer) = audio_rx.recv() => {
                if discard_next_audio {
                    discard_next_audio = false;
                    let _ = state_tx.send(DaemonState::Idle);
                    continue;
                }

                if *state_tx.borrow() != DaemonState::Processing {
                    continue;
                }

                let config_clone = config.clone();
                let model_clone = Arc::clone(model);
                let injector_clone = Arc::clone(injector);
                let state_tx_clone = state_tx.clone();

                tokio::spawn(async move {
                    if let Err(e) = run_inference(
                        &config_clone,
                        &model_clone,
                        &injector_clone,
                        &state_tx_clone,
                        audio_buffer,
                    ).await {
                        tracing::error!("Erro durante a inferência: {}", e);
                        let _ = state_tx_clone.send(DaemonState::Idle);
                    }
                });
            }
        }
    }
}
