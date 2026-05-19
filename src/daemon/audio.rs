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

    pub fn record_to_completion(
        config: AudioConfig,
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
        let max_samples =
            config.max_recording_secs as usize * native_sample_rate as usize * channels as usize;

        // Fila lock-free para garantir ausência de buffer overrun no ALSA/PipeWire
        let rb = HeapRb::<f32>::new(max_samples);
        let (prod, mut cons) = rb.split();

        let stream = stream::build_stream(&device, &stream_config, sample_format, prod)?;

        stream.play()?;
        info!("Captura iniciada.");

        let max_duration = Duration::from_secs(config.max_recording_secs);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(poll_interval);
            if STOP_REQUESTED.load(Ordering::Relaxed) {
                break;
            }
            if start.elapsed() >= max_duration {
                warn!(
                    "Tempo máximo atingido ({}s). Encerrando.",
                    config.max_recording_secs
                );
                break;
            }
        }

        drop(stream);

        // Extrai as amostras consumindo do RingBuffer
        let mut raw_buffer = Vec::new();
        while let Some(sample) = cons.try_pop() {
            raw_buffer.push(sample);
        }

        info!("Captura encerrada: {} amostras brutas", raw_buffer.len());

        let processed = dsp::process_audio(raw_buffer, channels, native_sample_rate, target_rate);

        info!("Áudio final: {} amostras a 16kHz", processed.len());

        audio_tx
            .blocking_send(processed)
            .map_err(|_| anyhow::anyhow!("Canal de áudio fechado."))?;

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
