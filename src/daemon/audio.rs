/// audio/ — Captura e processamento de áudio via cpal.
///
/// Responsabilidades:
/// - Abrir o microfone configurado apenas quando solicitado
/// - Capturar amostras em f32 mono 16kHz (formato exigido pelo Whisper)
/// - Fazer resample automático se o dispositivo não suportar 16kHz nativamente
/// - Converter stereo → mono quando necessário
/// - Manter o buffer de áudio exclusivamente em memória (LGPD)
/// - Encerrar a captura ao receber sinal via flag atômica
/// - Enviar o buffer completo ao loop principal via canal mpsc
///
/// A função `record_to_completion` é projetada para rodar em
/// `tokio::task::spawn_blocking` — ela bloqueia a thread até a gravação terminar.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::AudioConfig;

mod device;
mod dsp;
mod stream;

/// Flag atômica compartilhada entre o loop principal (que chama `signal_stop`)
/// e o callback de áudio cpal (que a consulta a cada chunk).
///
/// Usar `static` aqui é necessário porque o callback do cpal não é `Send + 'static`
/// de forma genérica — a flag estática resolve o lifetime sem unsafe.
pub(super) static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct AudioCapture;

impl AudioCapture {
    /// Sinaliza que a gravação deve ser encerrada.
    /// Chamado pelo loop principal (daemon/state_machine.rs) ao receber Toggle/Stop via IPC.
    pub fn signal_stop() {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
        debug!("Sinal de parada de áudio enviado.");
    }

    /// Captura áudio do microfone até `signal_stop()` ser chamado ou o tempo
    /// máximo ser atingido. Ao terminar, envia o buffer completo pelo canal.
    ///
    /// Esta função BLOQUEIA a thread corrente — sempre chame via `spawn_blocking`.
    ///
    /// Retorna `Ok(())` em caso de sucesso ou encerramento normal.
    /// Erros fatais (dispositivo não encontrado, formato inválido) retornam `Err`.
    pub fn record_to_completion(
        config: AudioConfig,
        audio_tx: mpsc::Sender<Vec<f32>>,
    ) -> anyhow::Result<()> {
        // Reseta a flag antes de começar (pode ter sido usada em gravação anterior)
        STOP_REQUESTED.store(false, Ordering::Relaxed);

        // --- Seleciona o host e o dispositivo ---
        let host = cpal::default_host();
        let device = device::select_device(&host, &config.device)?;
        let device_desc = device.description()?;
        info!("Dispositivo de áudio selecionado: {}", device_desc.name());

        // --- Negocia o formato de stream com o dispositivo ---
        let selected_config = device::negotiate_config(&device, config.sample_rate)?;
        let stream_config: StreamConfig = selected_config.config();
        let native_sample_rate = selected_config.sample_rate();
        let channels = selected_config.channels();
        let sample_format = selected_config.sample_format();

        info!(
            "Stream de áudio: {}Hz, {} canal(is), {:?} — alvo: {}Hz mono",
            native_sample_rate, channels, sample_format, config.sample_rate
        );

        // --- Buffer compartilhado entre callback e thread principal ---
        // Arc<Mutex<>> porque o callback cpal roda em thread de áudio separada
        // Pré-aloca exatamente para max_recording_secs (margem mínima para evitar realloc)
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
            config.max_recording_secs as usize * native_sample_rate as usize * channels as usize,
        )));

        let buffer_cb = Arc::clone(&buffer);
        let target_rate = config.sample_rate;
        let max_samples =
            config.max_recording_secs as usize * native_sample_rate as usize * channels as usize;

        // --- Monta o stream de acordo com o formato de amostra do dispositivo ---
        let stream = stream::build_stream(
            &device,
            &stream_config,
            sample_format,
            buffer_cb,
            max_samples,
        )?;

        stream.play()?;
        info!("Captura de áudio iniciada.");

        // --- Loop de espera: verifica a flag a cada 50ms ---
        // (50ms de latência de parada é imperceptível para o usuário)
        let max_duration = Duration::from_secs(config.max_recording_secs);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(poll_interval);

            if STOP_REQUESTED.load(Ordering::Relaxed) {
                info!("Parada solicitada — encerrando captura.");
                break;
            }

            if start.elapsed() >= max_duration {
                warn!(
                    "Tempo máximo de gravação atingido ({} segundos). Encerrando automaticamente.",
                    config.max_recording_secs
                );
                break;
            }
        }

        // Encerra o stream (para o callback de áudio)
        drop(stream);

        // --- Pós-processamento: resample + mixdown ---
        let raw_buffer = {
            let lock = buffer
                .lock()
                .map_err(|_| anyhow::anyhow!("Mutex de áudio envenenado"))?;
            lock.clone()
        };

        info!(
            "Captura encerrada: {} amostras brutas ({:.1}s)",
            raw_buffer.len(),
            raw_buffer.len() as f32 / (native_sample_rate as f32 * channels as f32)
        );

        // Mixdown stereo → mono (se necessário) e resample → 16kHz
        let processed = dsp::process_audio(raw_buffer, channels, native_sample_rate, target_rate);

        info!(
            "Áudio processado: {} amostras a 16kHz ({:.1}s)",
            processed.len(),
            processed.len() as f32 / target_rate as f32
        );

        // Envia o buffer para o loop principal via canal
        // Usa `blocking_send` porque estamos em contexto síncrono (spawn_blocking)
        audio_tx
            .blocking_send(processed)
            .map_err(|_| anyhow::anyhow!("Canal de áudio fechado — daemon encerrou?"))?;

        Ok(())
    }

    /// Lista os dispositivos de entrada disponíveis no sistema.
    /// Usado pelo subcomando `list-devices`.
    pub fn list_devices() -> anyhow::Result<Vec<String>> {
        let host = cpal::default_host();
        let mut names = Vec::new();

        for device in host.input_devices()? {
            if let Ok(desc) = device.description() {
                names.push(desc.name().to_string());
            }
        }

        Ok(names)
    }
}
