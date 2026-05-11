/// audio.rs — Captura de áudio via cpal (abstração sobre PipeWire/ALSA).
///
/// Responsabilidades:
/// - Abrir o microfone configurado apenas quando solicitado
/// - Capturar amostras em f32 mono 16kHz (formato exigido pelo Whisper)
/// - Fazer resample automático se o dispositivo não suportar 16kHz nativamente
/// - Converter stereo → mono quando necessário
/// - Manter o buffer de áudio exclusivamente em memória (LGPD)
/// - Encerrar a captura ao receber sinal via flag atômica
/// - Enviar o buffer completo ao loop principal via canal mpsc
///
/// A função `record_to_completion` é projetada para rodar em
/// `tokio::task::spawn_blocking` — ela bloqueia a thread até a gravação terminar.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::AudioConfig;

// =============================================================================
// Flag global de parada
// =============================================================================

/// Flag atômica compartilhada entre o loop principal (que chama `signal_stop`)
/// e o callback de áudio cpal (que a consulta a cada chunk).
///
/// Usar `static` aqui é necessário porque o callback do cpal não é `Send + 'static`
/// de forma genérica — a flag estática resolve o lifetime sem unsafe.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

// =============================================================================
// Struct pública
// =============================================================================

pub struct AudioCapture;

impl AudioCapture {
    /// Sinaliza que a gravação deve ser encerrada.
    /// Chamado pelo loop principal (daemon/mod.rs) ao receber Toggle/Stop via IPC.
    pub fn signal_stop() {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
        debug!("Sinal de parada de áudio enviado.");
    }

    /// Captura áudio do microfone até `signal_stop()` ser chamado ou o tempo
    /// máximo ser atingido. Ao terminar, envia o buffer completo pelo canal.
    ///
    /// Esta função BLOQUEIA a thread corrente — sempre chame via `spawn_blocking`.
    ///
    /// Retorna `Ok(())` em caso de sucesso ou encerramento normal.
    /// Erros fatais (dispositivo não encontrado, formato inválido) retornam `Err`.
    pub fn record_to_completion(
        config: AudioConfig,
        audio_tx: mpsc::Sender<Vec<f32>>,
    ) -> anyhow::Result<()> {
        // Reseta a flag antes de começar (pode ter sido usada em gravação anterior)
        STOP_REQUESTED.store(false, Ordering::Relaxed);

        // --- Seleciona o host e o dispositivo ---
        let host = cpal::default_host();

        let device = select_device(&host, &config.device)?;
        let device_desc = device.description()?;
        info!("Dispositivo de áudio selecionado: {}", device_desc.name());

        // --- Negocia o formato de stream com o dispositivo ---
        let (stream_config, native_sample_rate, channels) =
            negotiate_config(&device, config.sample_rate)?;

        info!(
            "Stream de áudio: {}Hz, {} canal(is) — alvo: {}Hz mono",
            native_sample_rate, channels, config.sample_rate
        );

        // --- Buffer compartilhado entre callback e thread principal ---
        // Arc<Mutex<>> porque o callback cpal roda em thread de áudio separada
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
            // Pré-aloca para config.max_recording_secs * taxa * canais
            // Evita realocações durante gravações longas
            (config.max_recording_secs as usize + 10) * native_sample_rate as usize * channels as usize,
        )));

        let buffer_cb = Arc::clone(&buffer);
        let target_rate = config.sample_rate;
        let max_samples = config.max_recording_secs as usize * native_sample_rate as usize * channels as usize;

        // --- Monta o stream de acordo com o formato de amostra do dispositivo ---
        let stream = build_stream(
            &device,
            &stream_config,
            buffer_cb,
            channels,
            max_samples,
        )?;

        stream.play()?;
        info!("Captura de áudio iniciada.");

        // --- Loop de espera: verifica a flag a cada 50ms ---
        // (50ms de latência de parada é imperceptível para o usuário)
        let max_duration = Duration::from_secs(config.max_recording_secs);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            std::thread::sleep(poll_interval);

            if STOP_REQUESTED.load(Ordering::Relaxed) {
                info!("Parada solicitada — encerrando captura.");
                break;
            }

            if start.elapsed() >= max_duration {
                warn!(
                    "Tempo máximo de gravação atingido ({} segundos). Encerrando automaticamente.",
                    config.max_recording_secs
                );
                break;
            }
        }

        // Encerra o stream (para o callback de áudio)
        drop(stream);

        // --- Pós-processamento: resample + mixdown ---
        let raw_buffer = {
            let lock = buffer.lock().map_err(|_| anyhow::anyhow!("Mutex de áudio envenenado"))?;
            lock.clone()
        };

        info!(
            "Captura encerrada: {} amostras brutas ({:.1}s)",
            raw_buffer.len(),
            raw_buffer.len() as f32 / (native_sample_rate as f32 * channels as f32)
        );

        // Mixdown stereo → mono (se necessário) e resample → 16kHz
        let processed = process_audio(raw_buffer, channels, native_sample_rate, target_rate);

        info!(
            "Áudio processado: {} amostras a 16kHz ({:.1}s)",
            processed.len(),
            processed.len() as f32 / target_rate as f32
        );

        // Envia o buffer para o loop principal via canal
        // Usa `blocking_send` porque estamos em contexto síncrono (spawn_blocking)
        audio_tx
            .blocking_send(processed)
            .map_err(|_| anyhow::anyhow!("Canal de áudio fechado — daemon encerrou?"))?;

        Ok(())
    }

    /// Lista os dispositivos de entrada disponíveis no sistema.
    /// Usado pelo subcomando `list-devices`.
    pub fn list_devices() -> anyhow::Result<Vec<String>> {
        let host = cpal::default_host();
        let mut names = Vec::new();

        for device in host.input_devices()? {
            if let Ok(desc) = device.description() {
                names.push(desc.name().to_string());
            }
        }

        Ok(names)
    }
}

// =============================================================================
// Helpers internos
// =============================================================================

/// Seleciona o dispositivo de entrada por nome ou usa o padrão.
fn select_device(host: &cpal::Host, device_name: &str) -> anyhow::Result<Device> {
    if device_name == "default" {
        return host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("Nenhum dispositivo de entrada padrão encontrado."));
    }

    // Busca pelo nome exato ou prefixo
    for device in host.input_devices()? {
        if let Ok(desc) = device.description() {
            let name = desc.name();
            if name.starts_with(device_name) {
                return Ok(device);
            }
        }
    }

    anyhow::bail!(
        "Dispositivo de áudio '{}' não encontrado. \
         Use `whisper-dictate list-devices` para ver as opções disponíveis.",
        device_name
    )
}

/// Negocia o melhor StreamConfig com o dispositivo.
///
/// Estratégia:
/// 1. Tenta 16kHz mono (ideal — sem necessidade de resample)
/// 2. Aceita qualquer taxa suportada (faremos resample em software)
/// 3. Prefere f32, mas aceita i16 ou u16 (converte no callback)
///
/// Retorna (StreamConfig, taxa_nativa, canais).
fn negotiate_config(
    device: &Device,
    preferred_rate: u32,
) -> anyhow::Result<(StreamConfig, u32, u16)> {
    let supported = device
        .supported_input_configs()
        .map_err(|e| anyhow::anyhow!("Erro ao consultar configs do dispositivo: {}", e))?
        .collect::<Vec<_>>();

    if supported.is_empty() {
        anyhow::bail!("Dispositivo não possui configurações de entrada suportadas.");
    }

    // Tenta encontrar uma config que suporte a taxa desejada
    for cfg_range in &supported {
        if cfg_range.min_sample_rate() <= preferred_rate
            && cfg_range.max_sample_rate() >= preferred_rate
        {
            let channels = cfg_range.channels().min(2); // mono ou stereo
            let config = StreamConfig {
                channels,
                sample_rate: preferred_rate,
                buffer_size: cpal::BufferSize::Default,
            };
            return Ok((config, preferred_rate, channels));
        }
    }

    // Fallback: usa a primeira config suportada (faremos resample)
    let best = &supported[0];
    let channels = best.channels().min(2);
    // Usa a taxa máxima suportada para melhor qualidade antes do resample
    let native_rate = best.max_sample_rate();

    let config = StreamConfig {
        channels,
        sample_rate: native_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    warn!(
        "Taxa {}Hz não suportada diretamente. Usando {}Hz com resample para {}Hz.",
        preferred_rate, native_rate, preferred_rate
    );

    Ok((config, native_rate, channels))
}

/// Constrói o stream de áudio com callback que acumula amostras no buffer.
///
/// O callback aceita qualquer SampleFormat e converte para f32 internamente.
fn build_stream(
    device: &Device,
    config: &StreamConfig,
    buffer: Arc<Mutex<Vec<f32>>>,
    _channels: u16,
    max_samples: usize,
) -> anyhow::Result<cpal::Stream> {
    // Tenta construir com f32 primeiro (sem conversão)
    let supported_formats: Vec<SampleFormat> = device
        .supported_input_configs()
        .map_err(|e| anyhow::anyhow!("Erro ao consultar configs do dispositivo: {}", e))?
        .map(|c| c.sample_format())
        .collect();

    let use_format = if supported_formats.contains(&SampleFormat::F32) {
        SampleFormat::F32
    } else if supported_formats.contains(&SampleFormat::I16) {
        SampleFormat::I16
    } else {
        SampleFormat::U8
    };

    let stream = match use_format {
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
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|&s| s as f32 / i16::MAX as f32)
                        .collect();
                    accumulate_samples(&converted, &buf, max_samples);
                },
                |e| error!("Erro no stream de áudio (i16): {}", e),
                None,
            )?
        }
        _ => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                config,
                move |data: &[u8], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 - 128.0) / 128.0)
                        .collect();
                    accumulate_samples(&converted, &buf, max_samples);
                },
                |e| error!("Erro no stream de áudio (u8): {}", e),
                None,
            )?
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

// =============================================================================
// Processamento de áudio pós-captura
// =============================================================================

/// Converte o buffer bruto (N canais, taxa nativa) para mono 16kHz.
///
/// 1. Mixdown: N canais → mono (média aritmética por frame)
/// 2. Resample: taxa nativa → 16kHz (interpolação linear)
///
/// Interpolação linear é adequada para fala — introduz mínimo artefato
/// audível e é muito mais rápida que resamplers de alta qualidade.
fn process_audio(raw: Vec<f32>, channels: u16, native_rate: u32, target_rate: u32) -> Vec<f32> {
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
