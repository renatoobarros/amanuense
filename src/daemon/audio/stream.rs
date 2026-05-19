use std::sync::atomic::Ordering;

use cpal::traits::DeviceTrait;
use cpal::{Device, SampleFormat, StreamConfig};
use ringbuf::traits::Producer;
use tracing::error;

use super::STOP_REQUESTED;

pub(super) fn build_stream<P>(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    mut producer: P,
) -> anyhow::Result<cpal::Stream>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| accumulate_samples(data, &mut producer),
            |e| error!("Erro no stream de áudio (f32): {}", e),
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                let converted: Vec<f32> =
                    data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                accumulate_samples(&converted, &mut producer);
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
                accumulate_samples(&converted, &mut producer);
            },
            |e| error!("Erro no stream de áudio (u16): {}", e),
            None,
        )?,
        other => anyhow::bail!("Formato de amostra não suportado: {:?}", other),
    };

    Ok(stream)
}

#[inline]
fn accumulate_samples<P>(data: &[f32], producer: &mut P)
where
    P: Producer<Item = f32>,
{
    if STOP_REQUESTED.load(Ordering::Relaxed) {
        return;
    }

    let written = producer.push_slice(data);
    if written < data.len() {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
    }
}
