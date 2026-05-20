use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperState};

use crate::config::{InferenceConfig, ModelConfig};
use crate::daemon::model::WhisperModel;

pub struct StreamingSession {
    _model: Arc<WhisperModel>,
    config: InferenceConfig,
    model_config: ModelConfig,
    state: WhisperState,
    audio_buffer: Vec<f32>,
    prompt_text: String,
    last_words: Vec<String>,
    committed_cursor: usize,
    window_samples: usize,
    overlap_samples: usize,
    previous_window_tail: Vec<String>,
    is_first_chunk_after_slide: bool,
}

// O Particionador Inteligente
// Separa o texto agrupando o espaço à palavra. Assim o Wayland digita os
// espaços corretamente e não quebra a formatação.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        if c.is_whitespace() && !current.is_empty() && !current.chars().all(|x| x.is_whitespace()) {
            words.push(current.clone());
            current.clear();
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

// O Normalizador Matemático
// Transforma " Passando" e " passando" em "passando".
// Isso garante que o Acordo Local cruze as palavras mesmo se o Whisper mudar a capitalização.
fn normalize(s: &str) -> String {
    let n: String = s
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    if n.is_empty() {
        s.trim().to_string()
    } else {
        n
    }
}

fn find_overlap(w1: &[String], w2: &[String]) -> usize {
    let n1 = w1.len();
    let n2 = w2.len();
    let max_overlap = n1.min(n2);

    for len in (1..=max_overlap).rev() {
        let suffix = &w1[n1 - len..];
        let prefix = &w2[..len];

        if suffix
            .iter()
            .zip(prefix.iter())
            .all(|(a, b)| normalize(a) == normalize(b))
        {
            return len;
        }
    }
    0
}

impl StreamingSession {
    pub fn new(
        model: Arc<WhisperModel>,
        config: InferenceConfig,
        model_config: ModelConfig,
    ) -> anyhow::Result<Self> {
        let state = model.create_state()?;
        let window_samples = (config.stream_window_secs as usize) * 16000;
        let overlap_samples = 16000 * 2; // 2 segundos rígidos de overlap do áudio

        Ok(Self {
            _model: model,
            config,
            model_config,
            state,
            audio_buffer: Vec::with_capacity(window_samples + 16000),
            prompt_text: String::new(),
            last_words: Vec::new(),
            committed_cursor: 0,
            window_samples,
            overlap_samples,
            previous_window_tail: Vec::new(),
            is_first_chunk_after_slide: false,
        })
    }

    pub fn process_chunk(&mut self, chunk: &[f32]) -> anyhow::Result<String> {
        self.audio_buffer.extend_from_slice(chunk);
        let current_words = self.run_inference()?;

        if self.is_first_chunk_after_slide {
            let overlap_len = find_overlap(&self.previous_window_tail, &current_words);
            self.committed_cursor = overlap_len;
            self.last_words = current_words;
            self.is_first_chunk_after_slide = false;
            return Ok(String::new());
        }

        let prefix_len = self
            .last_words
            .iter()
            .zip(current_words.iter())
            .take_while(|(a, b)| normalize(a) == normalize(b))
            .count();

        let mut delta_text = String::new();

        // A TRAVA DE SEGURANÇA: Atrasa a injeção em 1 palavra.
        // Nunca injeta a última palavra detectada, pois o fonema dela pode estar cortado
        // no limite dos 500ms. Espera a próxima rodada confirmar.
        let safe_prefix = prefix_len.saturating_sub(1);

        if safe_prefix > self.committed_cursor {
            let delta_words = &current_words[self.committed_cursor..safe_prefix];
            delta_text = delta_words.join("");
            self.committed_cursor = safe_prefix;
        }

        self.last_words = current_words;

        // Janela Deslizante
        if self.audio_buffer.len() >= self.window_samples {
            if self.committed_cursor > 0 {
                self.previous_window_tail = self.last_words[..self.committed_cursor].to_vec();
            } else {
                self.previous_window_tail.clear();
            }

            let committed_words = &self.last_words[..self.committed_cursor];
            self.prompt_text.push_str(&committed_words.join(""));

            if self.prompt_text.len() > 1000 {
                let start = self.prompt_text.len() - 1000;
                if let Some(idx) = self.prompt_text[start..].find(' ') {
                    self.prompt_text = self.prompt_text[start + idx..].to_string();
                } else {
                    self.prompt_text = self.prompt_text[start..].to_string();
                }
            }

            let keep_start = self.audio_buffer.len().saturating_sub(self.overlap_samples);
            self.audio_buffer = self.audio_buffer[keep_start..].to_vec();

            self.last_words.clear();
            self.committed_cursor = 0;
            self.is_first_chunk_after_slide = true;
        }

        Ok(delta_text)
    }

    pub fn flush(&mut self) -> anyhow::Result<String> {
        let mut delta_text = String::new();
        // No flush final, injetamos a cauda que sobrou sem atraso
        if self.last_words.len() > self.committed_cursor {
            let delta_words = &self.last_words[self.committed_cursor..];
            delta_text = delta_words.join("");
            self.committed_cursor = self.last_words.len();
        }
        Ok(delta_text)
    }

    fn run_inference(&mut self) -> anyhow::Result<Vec<String>> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        let lang = if self.model_config.language == "auto" {
            None
        } else {
            Some(self.model_config.language.as_str())
        };
        params.set_language(lang);
        params.set_n_threads(self.model_config.n_threads);
        params.set_token_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        let mut full_prompt = String::new();
        if let Some(p) = self.config.effective_prompt() {
            full_prompt.push_str(&p);
            full_prompt.push_str(" ");
        }
        if !self.prompt_text.is_empty() {
            full_prompt.push_str(&self.prompt_text);
        }

        if !full_prompt.is_empty() {
            params.set_initial_prompt(&full_prompt);
        }

        self.state
            .full(params, &self.audio_buffer)
            .map_err(|e| anyhow::anyhow!("Erro na inferência: {}", e))?;

        let mut full_text = String::new();
        let n_segments = self.state.full_n_segments();
        for i in 0..n_segments {
            if let Some(segment) = self.state.get_segment(i) {
                let text = segment.to_str().unwrap_or("");
                if !is_artifact(text.trim()) {
                    full_text.push_str(text);
                }
            }
        }

        Ok(split_words(&full_text))
    }
}

fn is_artifact(text: &str) -> bool {
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
    if text.is_empty() {
        return true;
    }
    for a in ARTIFACTS {
        if text == *a {
            return true;
        }
    }
    if text.chars().all(|c| !c.is_alphanumeric()) {
        return true;
    }
    false
}
