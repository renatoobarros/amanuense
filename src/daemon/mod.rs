/// daemon/mod.rs — Loop principal e máquina de estados do daemon.
///
/// Estados possíveis:
///
///   ┌────────┐  Toggle  ┌───────────┐  Toggle/Stop  ┌────────────┐
///   │  Idle  │ ───────► │ Recording │ ────────────► │ Processing │
///   └────────┘          └───────────┘               └────────────┘
///        ▲                                                 │
///        └─────────────────────────────────────────────────┘
///                        (inferência concluída)
///
/// Princípios LGPD:
/// - O buffer de áudio reside exclusivamente em memória (Vec<f32>)
/// - Nenhum dado de voz ou texto é gravado em disco em momento algum
/// - O buffer é zerado (Drop) ao retornar ao estado Idle
pub mod ipc;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch, Mutex};
use tracing::{error, info};

use crate::config::Config;
use crate::daemon::ipc::{DaemonState, IpcCommand};

// Módulos que serão implementados nas próximas partes
use crate::daemon::audio::AudioCapture;
use crate::daemon::model::WhisperModel;
use crate::daemon::transcriber::Transcriber;
use crate::output::clipboard::set_primary_selection;
use crate::output::injector::TextInjector;

pub mod audio;
pub mod model;
pub mod transcriber;

// =============================================================================
// Ponto de entrada do daemon
// =============================================================================

/// Inicia e executa o daemon completo.
/// Bloqueia até receber SIGTERM ou erro fatal.
pub async fn run(config: Config) -> anyhow::Result<()> {
    info!("Iniciando whisper-dictate daemon");

    // --- Carrega o modelo na GPU (operação mais custosa — feita uma única vez) ---
    info!(
        "Carregando modelo: {}",
        config.model.path
    );
    let model = Arc::new(
        WhisperModel::load(&config.model).map_err(|e| {
            anyhow::anyhow!("Falha ao carregar modelo Whisper: {}", e)
        })?
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

    info!("Daemon pronto. Aguardando comandos via: {}", socket_path.display());

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

// =============================================================================
// Loop principal
// =============================================================================

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
    loop {
        tokio::select! {
            // --- Comando recebido via IPC ---
            Some(cmd) = cmd_rx.recv() => {
                let current_state = *state_tx.borrow();

                match (cmd, current_state) {
                    // Toggle em Idle → inicia gravação
                    (IpcCommand::Toggle, DaemonState::Idle) => {
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
                            config,
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

                    // Stop em Processing → força retorno ao Idle sem injetar texto
                    (IpcCommand::Stop, DaemonState::Processing) => {
                        info!("Stop forçado durante processamento.");
                        state_tx.send(DaemonState::Idle)?;
                    }

                    _ => {}
                }
            }

            // --- Áudio completo recebido da task de captura ---
            Some(audio_buffer) = audio_rx.recv() => {
                // Transição para Processing já foi feita em stop_recording
                run_inference(
                    config,
                    model,
                    injector,
                    state_tx,
                    audio_buffer,
                    notification_handle,
                ).await?;
            }
        }
    }
}

// =============================================================================
// Ações de estado
// =============================================================================

async fn start_recording(
    config: &Config,
    audio_tx: mpsc::Sender<Vec<f32>>,
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) -> anyhow::Result<()> {
    info!("Iniciando captura de áudio.");

    // Lança task de captura de áudio (não bloqueante)
    let cfg_audio = config.audio.clone();
    let handle = tokio::task::spawn_blocking(move || {
        AudioCapture::record_to_completion(cfg_audio, audio_tx)
    });

    *capture_handle = Some(handle);
    state_tx.send(DaemonState::Recording)?;

    // Notificação de início (persistente até ser substituída)
    if config.notification.notify_on_start {
        *notification_handle = notify_start(&config.notification);
    }

    Ok(())
}

async fn stop_recording(
    config: &Config,
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) {
    info!("Encerrando captura de áudio.");

    // Sinaliza a task de captura para parar (via flag atômica em audio.rs)
    AudioCapture::signal_stop();

    // Aguarda a task encerrar e o áudio ser enviado pelo canal
    if let Some(h) = capture_handle.take() {
        let _ = h.await;
    }

    // Fecha a notificação de gravação
    if let Some(nh) = notification_handle.take() {
        let _ = nh.close();
    }

    // Transição: Recording → Processing
    let _ = state_tx.send(DaemonState::Processing);
}

async fn run_inference(
    config: &Config,
    model: &Arc<WhisperModel>,
    injector: &Arc<Mutex<TextInjector>>,
    state_tx: &watch::Sender<DaemonState>,
    audio_buffer: Vec<f32>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) -> anyhow::Result<()> {
    let sample_count = audio_buffer.len();
    let duration_secs = sample_count as f32 / 16000.0;
    info!(
        "Iniciando inferência: {:.1}s de áudio ({} amostras)",
        duration_secs, sample_count
    );

    // Clona o que precisamos mover para o thread de inferência
    let model = Arc::clone(model);
    let inf_config = config.inference.clone();

    // Inferência é CPU/GPU intensiva — executa em thread dedicada fora do runtime Tokio
    let result = tokio::task::spawn_blocking(move || {
        Transcriber::transcribe(&model, &inf_config, &audio_buffer)
    })
    .await;

    // O buffer de áudio é liberado aqui (moved para spawn_blocking e descartado ao fim)

    match result {
        Ok(Ok(text)) if !text.trim().is_empty() => {
            info!("Transcrição concluída: {} caracteres", text.len());

            let final_text = if config.output.newline_on_finish {
                format!("{}\n", text)
            } else {
                text.clone()
            };

            // Injeta no campo de texto com cursor ativo
            {
                let inj = injector.lock().await;
                if let Err(e) = inj.type_text(&final_text) {
                    error!("Falha ao injetar texto: {}", e);
                }
            }

            // Atualiza seleção primária do Wayland
            if config.output.primary_selection {
                if let Err(e) = set_primary_selection(&final_text) {
                    error!("Falha ao definir seleção primária: {}", e);
                }
            }

            // Notificação de conclusão com preview do texto
            if config.notification.notify_on_finish {
                notify_finish(&config.notification, &text);
            }
        }
        Ok(Ok(_)) => {
            info!("Inferência retornou texto vazio — nada a injetar.");
        }
        Ok(Err(e)) => {
            error!("Erro na inferência: {}", e);
        }
        Err(e) => {
            error!("Falha na task de inferência: {}", e);
        }
    }

    // Retorna ao Idle — modelo permanece na VRAM
    let _ = state_tx.send(DaemonState::Idle);
    info!("Daemon de volta ao estado Idle.");

    Ok(())
}

// =============================================================================
// Notificações
// =============================================================================

fn notify_start(cfg: &crate::config::NotificationConfig) -> Option<notify_rust::NotificationHandle> {
    use notify_rust::Notification;

    let timeout = if cfg.start_timeout_ms == 0 {
        notify_rust::Timeout::Never
    } else {
        notify_rust::Timeout::Milliseconds(cfg.start_timeout_ms)
    };

    match Notification::new()
        .summary(&cfg.start_message)
        .timeout(timeout)
        .show()
    {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!("Falha ao exibir notificação de início: {}", e);
            None
        }
    }
}

fn notify_finish(cfg: &crate::config::NotificationConfig, transcribed_text: &str) {
    use notify_rust::Notification;

    // Exibe um preview do texto transcrito no corpo da notificação
    let preview = if transcribed_text.len() > 120 {
        format!("{}…", &transcribed_text[..120])
    } else {
        transcribed_text.to_string()
    };

    if let Err(e) = Notification::new()
        .summary(&cfg.finish_message)
        .body(&preview)
        .timeout(notify_rust::Timeout::Milliseconds(cfg.finish_timeout_ms))
        .show()
    {
        tracing::warn!("Falha ao exibir notificação de conclusão: {}", e);
    }
}

// =============================================================================
// Shutdown limpo via SIGTERM
// =============================================================================

async fn setup_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate())
        .expect("Falha ao registrar handler de SIGTERM");
    let mut sigint = signal(SignalKind::interrupt())
        .expect("Falha ao registrar handler de SIGINT");

    tokio::select! {
        _ = sigterm.recv() => info!("SIGTERM recebido."),
        _ = sigint.recv()  => info!("SIGINT recebido."),
    }
}
