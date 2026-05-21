use std::path::PathBuf;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

use tokio::sync::{mpsc, watch};
use tracing::{error, info};

use crate::config::Config;
use crate::daemon::ipc::{self, DaemonState, IpcCommand};
use crate::daemon::model::WhisperModel;
use crate::daemon::notifications::{notify_error, notify_finish};
use crate::daemon::shutdown::setup_shutdown_signal;
use crate::daemon::state_machine::{TranscriptionEvent, start_recording, stop_recording};
use crate::output::clipboard::set_primary_selection;
use crate::output::injector::TextInjector;

pub async fn run(config: Config) -> anyhow::Result<()> {
    info!("Iniciando amanuense daemon");
    info!("Carregando modelo: {}", config.model.path);

    let model = Arc::new(
        WhisperModel::load(&config.model)
            .map_err(|e| anyhow::anyhow!("Falha ao carregar modelo: {}", e))?,
    );

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IpcCommand>(8);
    let (state_tx, state_rx) = watch::channel(DaemonState::Idle);

    let (injector_tx, injector_rx) = std::sync::mpsc::channel::<String>();
    let injector = Arc::new(Mutex::new(TextInjector::new()?));

    let typing_delay = config.output.typing_delay_ms;

    let inj_clone = Arc::clone(&injector);
    std::thread::spawn(move || {
        while let Ok(text) = injector_rx.recv() {
            if text.is_empty() {
                continue;
            }
            if let Ok(mut inj) = inj_clone.lock() {
                if let Err(e) = inj.type_text(&text, typing_delay) {
                    error!("Falha na injeção Wayland: {}", e);
                }
            }
        }
    });

    let socket_path: PathBuf = config.ipc.resolved_socket_path()?;

    // FASE 4: Capturamos o handle do servidor IPC
    let ipc_handle = ipc::start_server(socket_path.clone(), cmd_tx, state_rx.clone()).await?;

    let shutdown = setup_shutdown_signal();
    let mut capture_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut inference_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut notification_handle: Option<notify_rust::NotificationHandle> = None;
    let mut stop_flag: Option<Arc<AtomicBool>> = None;

    let (text_tx, mut text_rx) = mpsc::channel::<TranscriptionEvent>(100);

    info!("Daemon pronto via: {}", socket_path.display());

    tokio::select! {
        _ = shutdown => {
            info!("SIGTERM recebido. Encerrando daemon de forma limpa...");
            // FASE 4: Abortar a task do servidor IPC para evitar task leak de rede
            ipc_handle.abort();
        }
        _ = async {
            loop {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        let current_state = *state_tx.borrow();
                        match (cmd, current_state) {
                            (IpcCommand::Toggle, DaemonState::Idle) => {
                                let _ = start_recording(
                                    &config, &model, text_tx.clone(), &state_tx,
                                    &mut capture_handle, &mut inference_handle,
                                    &mut notification_handle, &mut stop_flag
                                ).await;
                            }
                            (IpcCommand::Toggle | IpcCommand::Stop, DaemonState::Recording) => {
                                stop_recording(
                                    &state_tx, &mut capture_handle, &mut inference_handle,
                                    &mut notification_handle, &mut stop_flag
                                ).await;
                            }
                            _ => {}
                        }
                    }
                    Some(event) = text_rx.recv() => {
                        match event {
                            TranscriptionEvent::Delta(delta) => {
                                let _ = injector_tx.send(delta);
                            }
                            TranscriptionEvent::Finished(last_delta, full_text) => {
                                let _ = injector_tx.send(last_delta);

                                if config.output.newline_on_finish {
                                    let _ = injector_tx.send("\n".to_string());
                                }

                                if config.output.primary_selection && !full_text.trim().is_empty() {
                                    if let Err(e) = set_primary_selection(&full_text) {
                                        error!("Falha ao definir seleção primária (clipboard): {}", e);
                                    }
                                }

                                if config.notification.notify_on_finish {
                                    notify_finish(&config.notification, &full_text);
                                }

                                let _ = state_tx.send(DaemonState::Idle);
                            }
                            TranscriptionEvent::Error(err) => {
                                error!("Erro na transcrição: {}", err);
                                notify_error(&config.notification, &err);
                                let _ = state_tx.send(DaemonState::Idle);
                            }
                        }
                    }
                }
            }
        } => {}
    }

    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    Ok(())
}
