use tracing::info;

use crate::config::InferenceConfig;

use super::WHISPER_SAMPLE_RATE;
use super::params::run_segment;
use super::postprocess::remove_overlap_prefix;

pub(super) struct SegmentRunOptions<'a> {
    pub(super) config: &'a InferenceConfig,
    pub(super) segment_samples: usize,
    pub(super) overlap_samples: usize,
    pub(super) prompt: Option<&'a str>,
    pub(super) language: Option<&'a str>,
    pub(super) n_threads: i32,
}

pub(super) fn transcribe_long(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    opts: &SegmentRunOptions<'_>,
) -> anyhow::Result<String> {
    let total_samples = audio.len();

    // Trava de segurança contra loop infinito. Deslocamento mínimo de 1 frame.
    let step = opts
        .segment_samples
        .saturating_sub(opts.overlap_samples)
        .max(1);

    let n_segments = {
        let mut count = 0;
        let mut pos = 0;
        while pos < total_samples {
            count += 1;
            pos += step;
        }
        count
    };

    info!("Transcrição longa: {} segmentos a processar.", n_segments);

    let mut all_parts: Vec<String> = Vec::with_capacity(n_segments);
    let mut pos = 0;
    let mut seg_index = 0;

    while pos < total_samples {
        let end = (pos + opts.segment_samples).min(total_samples);
        let chunk = &audio[pos..end];
        let is_last = end >= total_samples;

        info!(
            "Segmento {}/{}: {:.1}s–{:.1}s ({} amostras){}",
            seg_index + 1,
            n_segments,
            pos as f32 / WHISPER_SAMPLE_RATE as f32,
            end as f32 / WHISPER_SAMPLE_RATE as f32,
            chunk.len(),
            if is_last { " [último]" } else { "" },
        );

        let text = run_segment(
            state,
            opts.config,
            chunk,
            opts.prompt,
            opts.language,
            opts.n_threads,
        )?;

        let text = text.trim().to_string();

        if !text.is_empty() {
            let cleaned = if seg_index > 0 && !all_parts.is_empty() && opts.overlap_samples > 0 {
                remove_overlap_prefix(&all_parts, &text)
            } else {
                text.clone()
            };

            if !cleaned.is_empty() {
                all_parts.push(cleaned);
            }
        }

        if is_last {
            break;
        }

        pos += step;
        seg_index += 1;
    }

    let result = all_parts.join(" ");
    info!(
        "Transcrição concluída: {} segmentos → {} caracteres",
        n_segments,
        result.len()
    );

    Ok(result)
}
