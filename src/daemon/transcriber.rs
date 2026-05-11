/// transcriber.rs — Motor de inferência Whisper com suporte a áudio longo.
///
/// Estratégia para gravações longas (>30s):
///
///   O Whisper opera em janelas de no máximo 30 segundos de áudio.
///   Para gravações longas, o áudio é dividido em segmentos sobrepostos:
///
///   Áudio:  |════════════════════════════════════════════════════════|
///   Seg 1:  |══════════════════════════════|  (28s)
///   Seg 2:                        |══════════════════════════════|   (28s, overlap 2s)
///   Seg 3:                                          |════════════|   (restante)
///
///   O texto de cada segmento é coletado e os resultados são concatenados,
///   usando os últimos tokens do segmento anterior como contexto (`n_past`)
///   para manter coerência entre segmentos.
///
/// LGPD: o buffer de áudio é recebido por referência e nunca é copiado
/// para disco. Apenas o texto transcrito trafega para fora deste módulo.
use whisper_rs::{FullParams, SamplingStrategy};
use tracing::{debug, info, warn};

use crate::config::InferenceConfig;
use crate::daemon::model::WhisperModel;

/// Taxa de amostragem fixada pelo Whisper (não alterável)
const WHISPER_SAMPLE_RATE: usize = 16_000;

/// Tamanho máximo de janela do Whisper em amostras (30s × 16kHz)
const WHISPER_MAX_SAMPLES: usize = 30 * WHISPER_SAMPLE_RATE;

// =============================================================================
// Struct pública
// =============================================================================

pub struct Transcriber;

impl Transcriber {
    /// Transcreve o buffer de áudio completo em texto.
    ///
    /// Lida automaticamente com gravações de qualquer duração:
    /// - Curtas (≤ segment_duration_secs): uma única chamada de inferência
    /// - Longas (> segment_duration_secs): múltiplas chamadas segmentadas
    ///
    /// Retorna o texto completo consolidado, ou erro em falha de inferência.
    pub fn transcribe(
        model: &WhisperModel,
        config: &InferenceConfig,
        audio: &[f32],
    ) -> anyhow::Result<String> {
        let total_samples = audio.len();
        let segment_samples = (config.segment_duration_secs as usize) * WHISPER_SAMPLE_RATE;
        let overlap_samples = (config.segment_overlap_secs as usize) * WHISPER_SAMPLE_RATE;

        // Garante que segmento não ultrapassa o limite do Whisper
        let segment_samples = segment_samples.min(WHISPER_MAX_SAMPLES);

        info!(
            "Iniciando transcrição: {:.1}s de áudio | segmentos de {}s com {}s de overlap",
            total_samples as f32 / WHISPER_SAMPLE_RATE as f32,
            config.segment_duration_secs,
            config.segment_overlap_secs,
        );

        if total_samples == 0 {
            warn!("Buffer de áudio vazio — nada a transcrever.");
            return Ok(String::new());
        }

        // --- Cria estado de inferência (isolado por sessão) ---
        let mut state = model.create_state()?;

        // --- Monta o prompt efetivo ---
        let prompt = config.effective_prompt();
        if let Some(ref p) = prompt {
            debug!("Prompt efetivo: \"{}\"", p);
        }

        // --- Decide entre transcrição simples ou segmentada ---
        if total_samples <= segment_samples {
            // Áudio curto: uma única inferência
            let text = run_segment(
                &mut state,
                config,
                audio,
                prompt.as_deref(),
                0,        // n_past = 0 (primeira e única chamada)
                true,     // último segmento
            )?;
            Ok(text.trim().to_string())
        } else {
            // Áudio longo: inferência segmentada
            transcribe_long(
                &mut state,
                config,
                audio,
                segment_samples,
                overlap_samples,
                prompt.as_deref(),
            )
        }
    }
}

// =============================================================================
// Inferência segmentada para áudio longo
// =============================================================================

/// Divide o áudio em segmentos sobrepostos e transcreve cada um.
/// Concatena os resultados de forma coerente.
fn transcribe_long(
    state: &mut whisper_rs::WhisperState,
    config: &InferenceConfig,
    audio: &[f32],
    segment_samples: usize,
    overlap_samples: usize,
    prompt: Option<&str>,
) -> anyhow::Result<String> {
    let total_samples = audio.len();
    let step = segment_samples.saturating_sub(overlap_samples);

    // Pré-calcula o número de segmentos para logging
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
    let mut n_past = 0i32;
    let mut pos = 0;
    let mut seg_index = 0;

    while pos < total_samples {
        let end = (pos + segment_samples).min(total_samples);
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
            config,
            chunk,
            prompt,
            n_past,
            is_last,
        )?;

        let text = text.trim().to_string();

        if !text.is_empty() {
            // Para segmentos com overlap: remove a parte duplicada do início.
            // Estratégia simples e robusta: se este não é o primeiro segmento,
            // tentamos remover o sufixo do segmento anterior que se sobrepõe.
            let cleaned = if seg_index > 0 && !all_parts.is_empty() && overlap_samples > 0 {
                remove_overlap_prefix(&all_parts, &text)
            } else {
                text.clone()
            };

            if !cleaned.is_empty() {
                all_parts.push(cleaned);
            }
        }

        // Atualiza n_past para preservar contexto no próximo segmento.
        // Limita ao máximo configurado para não sobrecarregar a atenção.
        n_past = (n_past + config.n_past_tokens).min(config.n_past_tokens * 4);

        if is_last {
            break;
        }

        // Avança com overlap: recua `overlap_samples` para o próximo segmento
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

// =============================================================================
// Inferência de um único segmento
// =============================================================================

/// Executa a inferência do Whisper em um único chunk de áudio.
///
/// Parâmetros:
/// - `state`: estado de inferência reutilizado entre segmentos
/// - `config`: configuração de inferência
/// - `audio`: slice de amostras f32 a 16kHz (≤ 30s)
/// - `prompt`: prompt de contexto inicial (None = sem prompt)
/// - `n_past`: tokens de contexto anteriores a preservar
/// - `is_last`: indica se é o último segmento (ativa single_segment=false)
fn run_segment(
    state: &mut whisper_rs::WhisperState,
    config: &InferenceConfig,
    audio: &[f32],
    prompt: Option<&str>,
    n_past: i32,
    _is_last: bool,
) -> anyhow::Result<String> {
    // --- Configura parâmetros da inferência ---
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

    // Idioma forçado (pt = Português do Brasil)
    // "auto" é deliberadamente evitado por reduzir precisão e aumentar latência
    params.set_language(Some("pt"));

    // Prompt de contexto (vocabulário técnico + instruções de estilo)
    if let Some(p) = prompt {
        params.set_initial_prompt(p);
    }

    // Contexto de sessão anterior (coerência entre segmentos longos)
    params.set_n_past(n_past);

    // Threading: usa os n_threads configurados
    params.set_n_threads(config.n_past_tokens.max(1)); // reutiliza campo disponível

    // Desabilita timestamps por token (não necessários, reduz overhead)
    params.set_token_timestamps(false);

    // Suprime tokens especiais no output (remove [BLANK_AUDIO], etc.)
    params.set_suppress_blank(true);
    params.set_suppress_non_speech_tokens(true);

    // Sem tradução — queremos transcrição direta em pt
    params.set_translate(false);

    // Desabilita saída no stderr do whisper.cpp (silencioso em produção)
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    // --- Executa a inferência ---
    state.full(params, audio).map_err(|e| {
        anyhow::anyhow!("Erro na inferência Whisper: {:?}", e)
    })?;

    // --- Coleta o texto de todos os segmentos retornados ---
    let n_segments = state.full_n_segments().map_err(|e| {
        anyhow::anyhow!("Erro ao obter número de segmentos: {:?}", e)
    })?;

    debug!("Segmentos internos do Whisper: {}", n_segments);

    let mut output = String::new();

    for i in 0..n_segments {
        let text = state.full_get_segment_text(i).map_err(|e| {
            anyhow::anyhow!("Erro ao obter texto do segmento {}: {:?}", i, e)
        })?;

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

// =============================================================================
// Helpers
// =============================================================================

/// Retorna `true` para textos que são artefatos conhecidos do Whisper
/// (gerados quando há silêncio ou ruído de fundo sem fala).
fn should_skip_segment(text: &str) -> bool {
    // Lista de artefatos comuns do Whisper em silêncio
    const ARTIFACTS: &[&str] = &[
        "[BLANK_AUDIO]",
        "[blank_audio]",
        "(silêncio)",
        "(Silêncio)",
        "[Music]",
        "[music]",
        "[Música]",
        "[música]",
        "[Applause]",
        "[applause]",
        "(música)",
        "(Música)",
        "...",
    ];

    let t = text.trim();
    if t.is_empty() {
        return true;
    }

    for artifact in ARTIFACTS {
        if t == *artifact {
            return true;
        }
    }

    // Ignora segmentos que são apenas pontuação
    if t.chars().all(|c| !c.is_alphanumeric()) {
        return true;
    }

    false
}

/// Remove do início de `current` a parte que já apareceu no final de `previous_parts`.
///
/// Estratégia: pega as últimas N palavras do texto acumulado até agora e
/// verifica se `current` começa com elas. Se sim, remove essa duplicata.
///
/// Isso lida com o fato de que o Whisper pode repetir palavras do contexto
/// anterior no início de um novo segmento com overlap.
fn remove_overlap_prefix(previous_parts: &[String], current: &str) -> String {
    // Tenta matches de 8, 4 e 2 palavras (do mais específico para o mais permissivo)
    for n_words in [8usize, 4, 2] {
        let suffix = last_n_words_of_parts(previous_parts, n_words);
        if suffix.is_empty() {
            continue;
        }

        // Busca case-insensitive pelo sufixo no início do segmento atual
        let current_lower = current.to_lowercase();
        let suffix_lower = suffix.to_lowercase();

        if let Some(pos) = current_lower.find(&suffix_lower) {
            if pos < current.len() / 3 {
                // O match está no primeiro terço — provavelmente é overlap real
                let after = current[pos + suffix.len()..].trim();
                if !after.is_empty() {
                    debug!(
                        "Overlap removido ({} palavras): '{}' | restante: '{}'",
                        n_words, suffix, after
                    );
                    return after.to_string();
                }
            }
        }
    }

    // Sem overlap detectado — retorna o texto inteiro
    current.to_string()
}

/// Extrai as últimas `n` palavras da concatenação de `parts`.
fn last_n_words_of_parts(parts: &[String], n: usize) -> String {
    let combined = parts.join(" ");
    let words: Vec<&str> = combined.split_whitespace().collect();
    if words.len() < n {
        return combined;
    }
    words[words.len() - n..].join(" ")
}
