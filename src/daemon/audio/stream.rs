use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use cpal::traits::DeviceTrait;
use cpal::{Device, SampleFormat, StreamConfig};
use tracing::error;

use super::STOP_REQUESTED;

/// Constrói o stream de áudio com callback que acumula amostras no buffer.
///
/// O callback aceita qualquer SampleFormat e converte para f32 internamente.
pub(super) fn build_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    buffer: Arc<Mutex<Vec<f32>>>,
    max_samples: usize,
) -> anyhow::Result<cpal::Stream> {
    let stream = match sample_format {
        SampleFormat::F32 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                config,
                move |data: &[f32], _| {
                    accumulate_samples(data, &buf, max_samples);
                },
                |e| error!("Erro no stream de áudio (f32): {}", e),
                None,
            )?
        }
        SampleFormat::I16 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                config,
                move |data: &[i16], _| {
                    let converted: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    accumulate_samples(&converted, &buf, max_samples);
                },
                |e| error!("Erro no stream de áudio (i16): {}", e),
                None,
            )?
        }
        SampleFormat::U16 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                config,
                move |data: &[u16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    accumulate_samples(&converted, &buf, max_samples);
                },
                |e| error!("Erro no stream de áudio (u16): {}", e),
                None,
            )?
        }
        other => {
            anyhow::bail!(
                "Formato de amostra não suportado no stream selecionado: {:?}",
                other
            )
        }
    };

    Ok(stream)
}

/// Callback interno: adiciona amostras ao buffer compartilhado.
/// Para automaticamente quando o limite de amostras é atingido.
#[inline]
fn accumulate_samples(data: &[f32], buffer: &Arc<Mutex<Vec<f32>>>, max_samples: usize) {
    if STOP_REQUESTED.load(Ordering::Relaxed) {
        return; // Não acumula após sinal de parada
    }

    if let Ok(mut buf) = buffer.try_lock() {
        let remaining = max_samples.saturating_sub(buf.len());
        if remaining == 0 {
            // Sinaliza parada automática por limite de tempo
            STOP_REQUESTED.store(true, Ordering::Relaxed);
            return;
        }
        let to_add = data.len().min(remaining);
        buf.extend_from_slice(&data[..to_add]);
    }
    // Se try_lock falhar, simplesmente descarta o chunk (< 1ms de áudio perdido)
}
