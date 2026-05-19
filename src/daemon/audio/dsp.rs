use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};
use tracing::warn;

pub(super) struct AudioProcessor {
    channels: usize,
    native_rate: u32,
    target_rate: u32,
    resampler: Option<Async<f32>>,
}

impl AudioProcessor {
    pub fn new(channels: u16, native_rate: u32, target_rate: u32) -> Self {
        let resampler = if native_rate != target_rate {
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };

            match Async::<f32>::new_sinc(
                target_rate as f64 / native_rate as f64,
                2.0,
                &params,
                4096,
                1,
                FixedAsync::Input,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    warn!(
                        "Falha ao instanciar o resampler Sinc ({}). Fallback linear ativado.",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        Self {
            channels: channels as usize,
            native_rate,
            target_rate,
            resampler,
        }
    }

    pub fn process(&mut self, raw: &[f32]) -> Vec<f32> {
        if raw.is_empty() {
            return Vec::new();
        }

        // Mixdown de N canais para Mono
        let mono: Vec<f32> = if self.channels == 1 {
            raw.to_vec()
        } else {
            raw.chunks_exact(self.channels)
                .map(|frame| frame.iter().sum::<f32>() / self.channels as f32)
                .collect()
        };

        if self.native_rate == self.target_rate {
            return mono;
        }

        if let Some(resampler) = &mut self.resampler {
            let in_frames = mono.len();

            let input_adapter = match InterleavedSlice::new(&mono, 1, in_frames) {
                Ok(adapter) => adapter,
                Err(_) => return self.linear_resample(&mono),
            };

            let out_frames = resampler.process_all_needed_output_len(in_frames);
            let mut out_vec = vec![0.0f32; out_frames];

            let mut output_adapter = match InterleavedSlice::new_mut(&mut out_vec, 1, out_frames) {
                Ok(adapter) => adapter,
                Err(_) => return self.linear_resample(&mono),
            };

            match resampler.process_all_into_buffer(
                &input_adapter,
                &mut output_adapter,
                in_frames,
                None,
            ) {
                Ok((_, frames_written)) => {
                    out_vec.truncate(frames_written);
                    out_vec
                }
                Err(_) => self.linear_resample(&mono),
            }
        } else {
            self.linear_resample(&mono)
        }
    }

    fn linear_resample(&self, mono: &[f32]) -> Vec<f32> {
        let ratio = self.native_rate as f64 / self.target_rate as f64;
        let output_len = (mono.len() as f64 / ratio) as usize;
        let mut resampled = Vec::with_capacity(output_len);

        for i in 0..output_len {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = pos - idx as f64;

            let s0 = mono.get(idx).copied().unwrap_or(0.0);
            let s1 = mono.get(idx + 1).copied().unwrap_or(s0);

            resampled.push(s0 + (s1 - s0) * frac as f32);
        }
        resampled
    }
}
