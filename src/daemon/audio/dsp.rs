use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};
use tracing::warn;

pub(super) fn process_audio(
    raw: Vec<f32>,
    channels: u16,
    native_rate: u32,
    target_rate: u32,
) -> Vec<f32> {
    let channels = channels as usize;

    // Mixdown de N canais para Mono
    let mono = if channels == 1 {
        raw
    } else {
        raw.chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect::<Vec<f32>>()
    };

    if native_rate == target_rate {
        return mono;
    }

    // Parâmetros para filtro Sinc de alta qualidade contra Aliasing
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    // rubato 2.0.0 exige definição estrita de tamanho assíncrono e passagem por referência
    let mut resampler = match Async::<f32>::new_sinc(
        target_rate as f64 / native_rate as f64,
        2.0,               // margem máxima de flutuação (segurança)
        &params,           // <-- Exigido como referência na v2.0.0
        4096,              // chunk buffer interno
        1,                 // canais
        FixedAsync::Input, // <-- Correção exata: A variante correta na v2.0.0 é `Input`
    ) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "Falha ao instanciar o resampler Sinc ({}). Fallback linear ativado.",
                e
            );
            return linear_resample(&mono, native_rate, target_rate);
        }
    };

    let in_frames = mono.len();

    // Novo padrão do rubato 2.0.0: Adapters para I/O
    let input_adapter = match InterleavedSlice::new(&mono, 1, in_frames) {
        Ok(adapter) => adapter,
        Err(e) => {
            warn!(
                "Erro no adapter de input do rubato ({}). Fallback linear ativado.",
                e
            );
            return linear_resample(&mono, native_rate, target_rate);
        }
    };

    // Pré-aloca o buffer de saída exato que a matemática do filtro Sinc exige
    let out_frames = resampler.process_all_needed_output_len(in_frames);
    let mut out_vec = vec![0.0f32; out_frames];

    let mut output_adapter = match InterleavedSlice::new_mut(&mut out_vec, 1, out_frames) {
        Ok(adapter) => adapter,
        Err(e) => {
            warn!(
                "Erro no adapter de output do rubato ({}). Fallback linear ativado.",
                e
            );
            return linear_resample(&mono, native_rate, target_rate);
        }
    };

    // Processa a totalidade da amostra em uma única etapa coordenada
    match resampler.process_all_into_buffer(&input_adapter, &mut output_adapter, in_frames, None) {
        Ok((_, frames_written)) => {
            out_vec.truncate(frames_written);
            out_vec
        }
        Err(e) => {
            warn!(
                "Falha ao aplicar resample no rubato ({}). Fallback linear ativado.",
                e
            );
            linear_resample(&mono, native_rate, target_rate)
        }
    }
}

/// Fallback linear antigo. Extremamente rápido, tolerância a falhas.
/// Mantido estritamente para o caso de restrições de memória explodirem a alocação do Sinc.
fn linear_resample(mono: &[f32], native_rate: u32, target_rate: u32) -> Vec<f32> {
    let ratio = native_rate as f64 / target_rate as f64;
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
