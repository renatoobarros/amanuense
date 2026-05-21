use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::DeviceTrait;
use cpal::{Device, SampleFormat, StreamConfig};
use ringbuf::traits::Producer;
use tracing::{error, warn};

pub(super) fn build_stream<P>(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    mut producer: P,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let sf1 = stop_flag.clone();
    let sf2 = stop_flag.clone();
    let sf3 = stop_flag.clone();

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| accumulate_samples(data, &mut producer, &sf1),
            |e| error!("Erro no stream de áudio (f32): {}", e),
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                let converted: Vec<f32> =
                    data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                accumulate_samples(&converted, &mut producer, &sf2);
            },
            |e| error!("Erro no stream de áudio (i16): {}", e),
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                let converted: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                    .collect();
                accumulate_samples(&converted, &mut producer, &sf3);
            },
            |e| error!("Erro no stream de áudio (u16): {}", e),
            None,
        )?,
        other => anyhow::bail!("Formato de amostra não suportado: {:?}", other),
    };
    Ok(stream)
}

#[inline]
fn accumulate_samples<P>(data: &[f32], producer: &mut P, stop_flag: &AtomicBool)
where
    P: Producer<Item = f32>,
{
    if stop_flag.load(Ordering::Relaxed) {
        return;
    }

    let written = producer.push_slice(data);
    if written < data.len() {
        let dropped = data.len() - written;
        // FASE 2: Não abortamos mais a gravação (sem STOP_REQUESTED = true).
        // Apenas descartamos o excesso, registramos via telemetria (log) e continuamos o stream.
        warn!(
            "Overrun no ringbuffer: {} frames descartados. Gravação continuada.",
            dropped
        );
    }
}
