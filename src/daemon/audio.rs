use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Split};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::AudioConfig;

mod device;
mod dsp;
mod stream;

pub struct AudioCapture;

impl AudioCapture {
    pub fn record_stream(
        config: AudioConfig,
        stream_step_ms: u32,
        audio_tx: mpsc::Sender<Vec<f32>>,
        stop_flag: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        stop_flag.store(false, Ordering::Relaxed);

        let host = cpal::default_host();
        let device = device::select_device(&host, &config.device)?;
        info!("Dispositivo selecionado: {}", device.description()?.name());

        let selected_config = device::negotiate_config(&device, config.sample_rate)?;
        let stream_config: StreamConfig = selected_config.config();
        let native_sample_rate = selected_config.sample_rate();
        let channels = selected_config.channels();
        let sample_format = selected_config.sample_format();
        let target_rate = config.sample_rate;

        let max_samples = (native_sample_rate * channels as u32 * 2) as usize;
        let rb = HeapRb::<f32>::new(max_samples);
        let (prod, mut cons) = rb.split();

        let stream = stream::build_stream(
            &device,
            &stream_config,
            sample_format,
            prod,
            stop_flag.clone(),
        )?;
        stream.play()?;
        info!("Captura de áudio contínua iniciada.");

        let mut processor = dsp::AudioProcessor::new(channels, native_sample_rate, target_rate);
        let step_samples = ((target_rate * stream_step_ms) / 1000) as usize;
        let mut accumulator = Vec::with_capacity(step_samples * 2);
        let mut raw_buffer = Vec::with_capacity(4096);

        let max_duration = Duration::from_secs(config.max_recording_secs);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(poll_interval);

            raw_buffer.clear();
            while let Some(sample) = cons.try_pop() {
                raw_buffer.push(sample);
            }

            if !raw_buffer.is_empty() {
                let processed = processor.process(&raw_buffer);
                accumulator.extend(processed);
            }

            while accumulator.len() >= step_samples {
                let chunk: Vec<f32> = accumulator.drain(..step_samples).collect();
                if audio_tx.blocking_send(chunk).is_err() {
                    warn!("Canal de áudio fechado prematuramente.");
                    stop_flag.store(true, Ordering::Relaxed);
                    break;
                }
            }

            if stop_flag.load(Ordering::Relaxed) {
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
