use std::sync::Arc;

use tokio::sync::{Mutex, mpsc, watch};
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

    // Lança task de captura de áudio (não bloqueante)
    let cfg_audio = config.audio.clone();
    let cfg_notification = config.notification.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let fallback_tx = audio_tx.clone();
        if let Err(e) = AudioCapture::record_to_completion(cfg_audio, audio_tx) {
            error!("Falha na captura de áudio: {}", e);
            // Notifica o usuário sobre o erro
            crate::daemon::notifications::notify_error(&cfg_notification, &e.to_string());
            // Garante desbloqueio do estado Processing caso a captura falhe antes de enviar áudio.
            let _ = fallback_tx.blocking_send(Vec::new());
        }
    });

    *capture_handle = Some(handle);
    state_tx.send(DaemonState::Recording)?;

    // Notificação de início (persistente até ser substituída)
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

    // Sinaliza a task de captura para parar (via flag atômica em audio.rs)
    AudioCapture::signal_stop();

    // Aguarda a task encerrar e o áudio ser enviado pelo canal
    if let Some(h) = capture_handle.take() {
        let _ = h.await;
    }

    // Fecha a notificação de gravação
    if let Some(nh) = notification_handle.take() {
        nh.close();
    }

    // Transição: Recording → Processing
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
    let duration_secs = sample_count as f32 / 16000.0;
    info!(
        "Iniciando inferência: {:.1}s de áudio ({} amostras)",
        duration_secs, sample_count
    );

    // Clona o que precisamos mover para o thread de inferência
    let model = Arc::clone(model);
    let model_config = config.model.clone();
    let inf_config = config.inference.clone();

    // Inferência é CPU/GPU intensiva — executa em thread dedicada fora do runtime Tokio
    let result = tokio::task::spawn_blocking(move || {
        Transcriber::transcribe(&model, &model_config, &inf_config, &audio_buffer)
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
                let mut inj = injector.lock().await;
                if let Err(e) = inj.type_text(&final_text) {
                    error!("Falha ao injetar texto: {}", e);
                }
            }

            // Atualiza seleção primária do Wayland
            if config.output.primary_selection
                && let Err(e) = set_primary_selection(&final_text)
            {
                error!("Falha ao definir seleção primária: {}", e);
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
