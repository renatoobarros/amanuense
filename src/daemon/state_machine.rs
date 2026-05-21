use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, watch};
use tracing::info;

use crate::config::Config;
use crate::daemon::audio::AudioCapture;
use crate::daemon::ipc::DaemonState;
use crate::daemon::model::WhisperModel;
use crate::daemon::notifications::notify_start;
use crate::daemon::transcriber::StreamingSession;

pub enum TranscriptionEvent {
    Delta(String),
    Finished(String, String), // (ultimo_delta, texto_completo)
    Error(String),
}

pub(super) async fn start_recording(
    config: &Config,
    model: &Arc<WhisperModel>,
    text_tx: mpsc::Sender<TranscriptionEvent>,
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    inference_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
    stop_flag: &mut Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    info!("Iniciando gravação e streaming.");

    let (audio_tx, mut audio_rx) = mpsc::channel(100);
    let cfg_audio = config.audio.clone();
    let step_ms = config.inference.stream_step_ms;

    // FASE 2: Cria o flag de controle de parada único da sessão
    let flag = Arc::new(AtomicBool::new(false));
    *stop_flag = Some(Arc::clone(&flag));

    let a_handle = tokio::task::spawn_blocking(move || {
        let _ = AudioCapture::record_stream(cfg_audio, step_ms, audio_tx, flag);
    });

    let m_clone = Arc::clone(model);
    let c_inf = config.inference.clone();
    let c_mod = config.model.clone();

    let i_handle = tokio::task::spawn_blocking(move || {
        let mut session = match StreamingSession::new(m_clone, c_inf, c_mod) {
            Ok(s) => s,
            Err(e) => {
                let _ = text_tx.blocking_send(TranscriptionEvent::Error(e.to_string()));
                return;
            }
        };

        let mut full_text = String::new();

        while let Some(chunk) = audio_rx.blocking_recv() {
            match session.process_chunk(&chunk) {
                Ok(delta) => {
                    full_text.push_str(&delta);
                    let _ = text_tx.blocking_send(TranscriptionEvent::Delta(delta));
                }
                Err(e) => {
                    let _ = text_tx.blocking_send(TranscriptionEvent::Error(e.to_string()));
                    return;
                }
            }
        }

        match session.flush() {
            Ok(delta) => {
                full_text.push_str(&delta);
                let _ = text_tx.blocking_send(TranscriptionEvent::Finished(delta, full_text));
            }
            Err(e) => {
                let _ = text_tx.blocking_send(TranscriptionEvent::Error(e.to_string()));
            }
        }
    });

    *capture_handle = Some(a_handle);
    *inference_handle = Some(i_handle);
    state_tx.send(DaemonState::Recording)?;

    if config.notification.notify_on_start {
        *notification_handle = notify_start(&config.notification);
    }

    Ok(())
}

pub(super) async fn stop_recording(
    state_tx: &watch::Sender<DaemonState>,
    capture_handle: &mut Option<tokio::task::JoinHandle<()>>,
    inference_handle: &mut Option<tokio::task::JoinHandle<()>>,
    notification_handle: &mut Option<notify_rust::NotificationHandle>,
    stop_flag: &mut Option<Arc<AtomicBool>>,
) {
    info!("Encerrando captura e consolidando.");

    // FASE 2: Sinaliza parada via Arc<AtomicBool>
    if let Some(flag) = stop_flag.take() {
        flag.store(true, Ordering::Relaxed);
    }

    // FASE 4: Correção estrutural antecipada para uso pleno dos estados.
    let _ = state_tx.send(DaemonState::Processing);

    if let Some(h) = capture_handle.take() {
        let _ = h.await;
    }

    // FASE 4: Correção do Task Leak antecipada. A task de inferência agora é aguardada.
    if let Some(h) = inference_handle.take() {
        let _ = h.await;
    }

    if let Some(nh) = notification_handle.take() {
        nh.close();
    }
}
