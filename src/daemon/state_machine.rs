use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};
use tracing::{error, info};

use crate::config::Config;
use crate::daemon::audio::AudioCapture;
use crate::daemon::ipc::DaemonState;
use crate::daemon::model::WhisperModel;
use crate::daemon::notifications::{notify_finish, notify_start};
use crate::daemon::transcriber::Transcriber;
use crate::output::clipboard::set_primary_selection;
use crate::output::injector::TextInjector;

pub(super) async fn start_recording(
    config: &Config,
    audio_tx: mpsc::Sender<Vec<f32>>,
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) -> anyhow::Result<()> {
    info!("Iniciando captura de áudio.");

    let cfg_audio = config.audio.clone();
    let cfg_notification = config.notification.clone();

    let handle = tokio::task::spawn_blocking(move || {
        let fallback_tx = audio_tx.clone();
        if let Err(e) = AudioCapture::record_to_completion(cfg_audio, audio_tx) {
            error!("Falha na captura de áudio: {}", e);
            crate::daemon::notifications::notify_error(&cfg_notification, &e.to_string());
            let _ = fallback_tx.blocking_send(Vec::new());
        }
    });

    *capture_handle = Some(handle);
    state_tx.send(DaemonState::Recording)?;

    if config.notification.notify_on_start {
        *notification_handle = notify_start(&config.notification);
    }

    Ok(())
}

pub(super) async fn stop_recording(
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
) {
    info!("Encerrando captura de áudio.");

    AudioCapture::signal_stop();

    if let Some(h) = capture_handle.take() {
        let _ = h.await;
    }

    if let Some(nh) = notification_handle.take() {
        nh.close();
    }

    let _ = state_tx.send(DaemonState::Processing);
}

pub(super) async fn run_inference(
    config: &Config,
    model: &Arc<WhisperModel>,
    injector: &Arc<Mutex<TextInjector>>,
    state_tx: &watch::Sender<DaemonState>,
    audio_buffer: Vec<f32>,
) -> anyhow::Result<()> {
    let sample_count = audio_buffer.len();
    info!("Iniciando inferência: {} amostras", sample_count);

    let model = Arc::clone(model);
    let model_config = config.model.clone();
    let inf_config = config.inference.clone();

    let result = tokio::task::spawn_blocking(move || {
        Transcriber::transcribe(&model, &model_config, &inf_config, &audio_buffer)
    })
    .await;

    match result {
        Ok(Ok(text)) if !text.trim().is_empty() => {
            info!("Transcrição concluída: {} caracteres", text.len());

            let final_text = if config.output.newline_on_finish {
                format!("{}\n", text)
            } else {
                text.clone()
            };

            let injector_clone = Arc::clone(injector);
            let config_output = config.output.clone();
            let config_notification = config.notification.clone();

            // Injeção de texto isolada da thread do Tokio
            tokio::task::spawn_blocking(move || {
                if let Ok(mut inj) = injector_clone.lock() {
                    if let Err(e) = inj.type_text(&final_text) {
                        error!("Falha ao injetar texto: {}", e);
                    }
                }

                if config_output.primary_selection {
                    if let Err(e) = set_primary_selection(&final_text) {
                        error!("Falha ao definir seleção primária: {}", e);
                    }
                }

                if config_notification.notify_on_finish {
                    notify_finish(&config_notification, &text);
                }
            })
            .await
            .unwrap_or_else(|e| error!("Falha na task de injeção: {}", e));
        }
        Ok(Ok(_)) => info!("Inferência retornou texto vazio."),
        Ok(Err(e)) => error!("Erro na inferência: {}", e),
        Err(e) => error!("Falha na task de inferência: {}", e),
    }

    let _ = state_tx.send(DaemonState::Idle);
    info!("Daemon de volta ao estado Idle.");

    Ok(())
}
