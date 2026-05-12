use tracing::debug;

/// Retorna `true` para textos que são artefatos conhecidos do Whisper
/// (gerados quando há silêncio ou ruído de fundo sem fala).
pub(super) fn should_skip_segment(text: &str) -> bool {
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
pub(super) fn remove_overlap_prefix(previous_parts: &[String], current: &str) -> String {
    // Tenta matches de 8, 4 e 2 palavras (do mais específico para o mais permissivo)
    for n_words in [8usize, 4, 2] {
        let suffix = last_n_words_of_parts(previous_parts, n_words);
        if suffix.is_empty() {
            continue;
        }

        // Busca case-insensitive pelo sufixo no início do segmento atual
        let current_lower = current.to_lowercase();
        let suffix_lower = suffix.to_lowercase();

        if let Some(pos) = current_lower.find(&suffix_lower)
            && pos < current.len() / 3
        {
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
