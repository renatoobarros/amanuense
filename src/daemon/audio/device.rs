use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, SampleFormat, SupportedStreamConfig};
use tracing::warn;

/// Seleciona o dispositivo de entrada por nome ou usa o padrão.
pub(super) fn select_device(host: &cpal::Host, device_name: &str) -> anyhow::Result<Device> {
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
         Use `amanuense list-devices` para ver as opções disponíveis.",
        device_name
    )
}

/// Negocia o melhor StreamConfig com o dispositivo.
///
/// Estratégia:
/// 1. Tenta a taxa alvo (16kHz no padrão) sem forçar formato/canais
/// 2. Prefere formatos estáveis para captura (f32, i16, u16)
/// 3. Se necessário, usa melhor fallback e aplica resample em software
///
/// Retorna a configuração efetivamente selecionada, incluindo sample_format.
pub(super) fn negotiate_config(
    device: &Device,
    preferred_rate: u32,
) -> anyhow::Result<SupportedStreamConfig> {
    let supported = device
        .supported_input_configs()
        .map_err(|e| anyhow::anyhow!("Erro ao consultar configs do dispositivo: {}", e))?
        .collect::<Vec<_>>();

    if supported.is_empty() {
        anyhow::bail!("Dispositivo não possui configurações de entrada suportadas.");
    }

    // Tenta encontrar uma config que suporte a taxa desejada, preferindo formatos estáveis.
    if let Some(cfg_range) = supported
        .iter()
        .filter(|cfg| {
            cfg.min_sample_rate() <= preferred_rate && cfg.max_sample_rate() >= preferred_rate
        })
        .min_by_key(|cfg| sample_format_rank(cfg.sample_format()))
    {
        return Ok(cfg_range.with_sample_rate(preferred_rate));
    }

    // Fallback: escolhe o melhor formato e a taxa máxima suportada (resample depois).
    let best = supported
        .iter()
        .max_by_key(|cfg| {
            (
                std::cmp::Reverse(sample_format_rank(cfg.sample_format())),
                cfg.max_sample_rate(),
            )
        })
        .ok_or_else(|| {
            anyhow::anyhow!("Dispositivo não possui configurações de entrada válidas.")
        })?;
    let selected = best.with_max_sample_rate();

    warn!(
        "Taxa {}Hz não suportada diretamente. Usando {}Hz com resample para {}Hz.",
        preferred_rate,
        selected.sample_rate(),
        preferred_rate
    );

    Ok(selected)
}

#[inline]
fn sample_format_rank(fmt: SampleFormat) -> usize {
    match fmt {
        SampleFormat::F32 => 0,
        SampleFormat::I16 => 1,
        SampleFormat::U16 => 2,
        _ => 10,
    }
}
