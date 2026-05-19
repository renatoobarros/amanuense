use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Split};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::AudioConfig;

mod device;
mod dsp;
mod stream;

pub(super) static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct AudioCapture;

impl AudioCapture {
    pub fn signal_stop() {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
        debug!("Sinal de parada de áudio enviado.");
    }

    pub fn record_stream(
        config: AudioConfig,
        stream_step_ms: u32,
        audio_tx: mpsc::Sender<Vec<f32>>,
    ) -> anyhow::Result<()> {
        STOP_REQUESTED.store(false, Ordering::Relaxed);

        let host = cpal::default_host();
        let device = device::select_device(&host, &config.device)?;
        info!("Dispositivo selecionado: {}", device.description()?.name());

        let selected_config = device::negotiate_config(&device, config.sample_rate)?;
        let stream_config: StreamConfig = selected_config.config();
        let native_sample_rate = selected_config.sample_rate();
        let channels = selected_config.channels();
        let sample_format = selected_config.sample_format();

        let target_rate = config.sample_rate;

        // O RingBuffer atua apenas como ponte entre a thread de áudio e a thread principal
        // O tamanho é fixo para evitar overruns (margem segura de ~2 segundos do áudio nativo)
        let max_samples = (native_sample_rate * channels as u32 * 2) as usize;
        let rb = HeapRb::<f32>::new(max_samples);
        let (prod, mut cons) = rb.split();

        let stream = stream::build_stream(&device, &stream_config, sample_format, prod)?;
        stream.play()?;
        info!("Captura de áudio contínua iniciada.");

        // Inicializa o processador de áudio stateful (mantém a fase do filtro Sinc viva)
        let mut processor = dsp::AudioProcessor::new(channels, native_sample_rate, target_rate);

        // O acumulador guarda o áudio processado até atingir o tamanho de corte
        let step_samples = ((target_rate * stream_step_ms) / 1000) as usize;
        let mut accumulator = Vec::with_capacity(step_samples * 2);
        let mut raw_buffer = Vec::with_capacity(4096);

        let max_duration = Duration::from_secs(config.max_recording_secs);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(poll_interval);

            // Drena o áudio novo que o microfone enviou nos últimos 50ms
            raw_buffer.clear();
            while let Some(sample) = cons.try_pop() {
                raw_buffer.push(sample);
            }

            if !raw_buffer.is_empty() {
                let processed = processor.process(&raw_buffer);
                accumulator.extend(processed);
            }

            // Sempre que o acumulador atinge o tamanho do passo (ex: 500ms = 8000 amostras)
            // recorta esse bloco exato e envia para a máquina de estados.
            while accumulator.len() >= step_samples {
                let chunk: Vec<f32> = accumulator.drain(..step_samples).collect();

                if audio_tx.blocking_send(chunk).is_err() {
                    warn!("Canal de áudio fechado prematuramente.");
                    STOP_REQUESTED.store(true, Ordering::Relaxed);
                    break;
                }
            }

            if STOP_REQUESTED.load(Ordering::Relaxed) {
                break;
            }

            if start.elapsed() >= max_duration {
                warn!(
                    "Tempo máximo de gravação atingido ({}s). Encerrando.",
                    config.max_recording_secs
                );
                break;
            }
        }

        drop(stream);

        // Drena e envia o "resíduo" final do áudio após o Stop ser solicitado
        raw_buffer.clear();
        while let Some(sample) = cons.try_pop() {
            raw_buffer.push(sample);
        }
        if !raw_buffer.is_empty() {
            let processed = processor.process(&raw_buffer);
            accumulator.extend(processed);
        }
        if !accumulator.is_empty() {
            let _ = audio_tx.blocking_send(accumulator);
        }

        info!("Captura de áudio contínua encerrada.");
        Ok(())
    }

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
