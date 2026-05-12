/// Converte o buffer bruto (N canais, taxa nativa) para mono 16kHz.
///
/// 1. Mixdown: N canais → mono (média aritmética por frame)
/// 2. Resample: taxa nativa → 16kHz (interpolação linear)
///
/// Interpolação linear é adequada para fala — introduz mínimo artefato
/// audível e é muito mais rápida que resamplers de alta qualidade.
pub(super) fn process_audio(
    raw: Vec<f32>,
    channels: u16,
    native_rate: u32,
    target_rate: u32,
) -> Vec<f32> {
    let channels = channels as usize;

    // --- 1. Mixdown N canais → mono ---
    let mono: Vec<f32> = if channels == 1 {
        raw // já é mono, evita cópia
    } else {
        raw.chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    // --- 2. Resample para 16kHz ---
    if native_rate == target_rate {
        return mono; // já está na taxa correta
    }

    let ratio = native_rate as f64 / target_rate as f64;
    let output_len = (mono.len() as f64 / ratio) as usize;
    let mut resampled = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        let frac = pos - idx as f64;

        let s0 = mono.get(idx).copied().unwrap_or(0.0);
        let s1 = mono.get(idx + 1).copied().unwrap_or(s0);

        // Interpolação linear entre amostras vizinhas
        resampled.push(s0 + (s1 - s0) * frac as f32);
    }

    resampled
}
