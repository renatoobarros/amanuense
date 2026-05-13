/// transcriber/ — Motor de inferência Whisper com suporte a áudio longo.
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
///   mantendo coerência por meio do estado compartilhado do Whisper e
///   controle do contexto máximo (`n_max_text_ctx`).
///
/// LGPD: o buffer de áudio é recebido por referência e nunca é copiado
/// para disco. Apenas o texto transcrito trafega para fora deste módulo.
use tracing::{debug, info, warn};

use crate::config::{InferenceConfig, ModelConfig};
use crate::daemon::model::WhisperModel;

mod params;
mod postprocess;
mod segmentation;

use params::run_segment;
use segmentation::{SegmentRunOptions, transcribe_long};

/// Taxa de amostragem fixada pelo Whisper (não alterável)
pub(super) const WHISPER_SAMPLE_RATE: usize = 16_000;

/// Tamanho máximo de janela do Whisper em amostras (30s × 16kHz)
pub(super) const WHISPER_MAX_SAMPLES: usize = 30 * WHISPER_SAMPLE_RATE;

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
        model_config: &ModelConfig,
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

        // --- Idioma e threading ---
        let language = model_config.language.trim();
        let language = if language.is_empty() || language == "auto" {
            None
        } else {
            Some(language)
        };
        let n_threads = model_config.n_threads.max(1);

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
                language,
                n_threads,
            )?;
            Ok(text.trim().to_string())
        } else {
            let opts = SegmentRunOptions {
                config,
                segment_samples,
                overlap_samples,
                prompt: prompt.as_deref(),
                language,
                n_threads,
            };

            // Áudio longo: inferência segmentada
            transcribe_long(&mut state, audio, &opts)
        }
    }
}
