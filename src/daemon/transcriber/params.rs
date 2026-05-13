use tracing::debug;
use whisper_rs::{FullParams, SamplingStrategy};

use crate::config::InferenceConfig;

use super::postprocess::should_skip_segment;

/// Executa a inferência do Whisper em um único chunk de áudio.
///
/// Parâmetros:
/// - `state`: estado de inferência reutilizado entre segmentos
/// - `config`: configuração de inferência
/// - `audio`: slice de amostras f32 a 16kHz (≤ 30s)
/// - `prompt`: prompt de contexto inicial (None = sem prompt)
/// - `n_past`: tokens de contexto anteriores a preservar
/// - `is_last`: indica se é o último segmento (ativa single_segment=false)
pub(super) fn run_segment(
    state: &mut whisper_rs::WhisperState,
    config: &InferenceConfig,
    audio: &[f32],
    prompt: Option<&str>,
    language: Option<&str>,
    n_threads: i32,
) -> anyhow::Result<String> {
    // --- Configura parâmetros da inferência ---
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

    // Idioma (None = auto)
    if language.is_none() {
        params.set_detect_language(true);
    }
    params.set_language(language);

    // Prompt de contexto (vocabulário técnico + instruções de estilo)
    if let Some(p) = prompt {
        params.set_initial_prompt(p);
    }

    // Contexto máximo de texto (limita o uso de contexto histórico)
    params.set_n_max_text_ctx(config.n_past_tokens.max(1));

    // Threading: usa os n_threads configurados
    params.set_n_threads(n_threads);

    // Desabilita timestamps por token (não necessários, reduz overhead)
    params.set_token_timestamps(false);

    // Suprime tokens especiais no output (remove [BLANK_AUDIO], etc.)
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);

    // Sem tradução — queremos transcrição direta em pt
    params.set_translate(false);

    // Desabilita saída no stderr do whisper.cpp (silencioso em produção)
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    // --- Executa a inferência ---
    state
        .full(params, audio)
        .map_err(|e| anyhow::anyhow!("Erro na inferência Whisper: {:?}", e))?;

    // --- Coleta o texto de todos os segmentos retornados ---
    let n_segments = state.full_n_segments();

    debug!("Segmentos internos do Whisper: {}", n_segments);

    let mut output = String::new();

    for i in 0..n_segments {
        let segment = state
            .get_segment(i)
            .ok_or_else(|| anyhow::anyhow!("Segmento {} fora do range", i))?;
        let text = segment
            .to_str()
            .map_err(|e| anyhow::anyhow!("Erro ao obter texto do segmento {}: {:?}", i, e))?;

        // Filtra artefatos comuns do Whisper em silêncio ou ruído
        let text = text.trim();
        if should_skip_segment(text) {
            debug!("Segmento interno {} ignorado (artefato): '{}'", i, text);
            continue;
        }

        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(text);
    }

    Ok(output)
}
