/// config.rs — Leitura e validação da configuração do usuário.
///
/// O arquivo de configuração é procurado, em ordem, nos seguintes locais:
///   1. Caminho passado via flag `--config` na linha de comando
///   2. $XDG_CONFIG_HOME/amanuense/config.toml
///   3. ~/.config/amanuense/config.toml
///
/// Nenhum dado é gravado em disco por este módulo — apenas leitura.
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{info, warn};

// =============================================================================
// Estruturas de configuração
// =============================================================================

/// Configuração completa do daemon, mapeada 1:1 com config.toml.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub model: ModelConfig,
    pub audio: AudioConfig,
    pub inference: InferenceConfig,
    pub output: OutputConfig,
    pub notification: NotificationConfig,
    pub ipc: IpcConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModelConfig {
    /// Caminho para o arquivo .bin do modelo GGML (aceita ~)
    pub path: String,

    /// Código de idioma forçado (ex: "pt", "en", "auto")
    pub language: String,

    /// Habilitar inferência na GPU
    pub use_gpu: bool,

    /// Índice da GPU (0 = primeira)
    pub gpu_device: i32,

    /// Threads de CPU para partes não-GPU
    pub n_threads: i32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AudioConfig {
    /// Nome do dispositivo de entrada ("default" = dispositivo padrão do sistema)
    pub device: String,

    /// Taxa de amostragem em Hz (o Whisper requer 16000)
    pub sample_rate: u32,

    /// Duração máxima de gravação em segundos (proteção contra gravações acidentais)
    pub max_recording_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InferenceConfig {
    /// Duração de cada segmento enviado ao Whisper (em segundos, máx 30)
    pub segment_duration_secs: u32,

    /// Sobreposição entre segmentos consecutivos (em segundos)
    pub segment_overlap_secs: u32,

    /// Prompt inicial para guiar o modelo no domínio correto
    pub initial_prompt: String,

    /// Prompt de sistema (concatenado ao initial_prompt)
    pub system_prompt: String,

    /// Tokens de contexto preservados entre segmentos em gravações longas
    pub n_past_tokens: i32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OutputConfig {
    /// Atualizar seleção primária do Wayland ao finalizar
    pub primary_selection: bool,

    /// Adicionar \n ao final do texto transcrito
    pub newline_on_finish: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NotificationConfig {
    /// Exibir notificação ao iniciar gravação
    pub notify_on_start: bool,

    /// Mensagem de início
    pub start_message: String,

    /// Exibir notificação ao finalizar
    pub notify_on_finish: bool,

    /// Título da notificação de fim
    pub finish_message: String,

    /// Timeout da notificação de início em ms (0 = persistente)
    pub start_timeout_ms: u32,

    /// Timeout da notificação de fim em ms
    pub finish_timeout_ms: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IpcConfig {
    /// Caminho do Unix Socket (vazio = padrão XDG)
    pub socket_path: String,
}

// =============================================================================
// Implementações
// =============================================================================

impl Config {
    /// Carrega e valida a configuração a partir do caminho fornecido.
    /// Se `path` for None, busca nos locais padrão XDG.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };

        info!("Carregando configuração de: {}", config_path.display());

        let raw = std::fs::read_to_string(&config_path).map_err(|e| {
            anyhow::anyhow!(
                "Não foi possível ler o arquivo de configuração em '{}': {}.\n\
                 Crie o arquivo ou passe --config <caminho>.",
                config_path.display(),
                e
            )
        })?;

        let mut config: Config = toml::from_str(&raw).map_err(|e| {
            anyhow::anyhow!("Erro ao interpretar '{}': {}", config_path.display(), e)
        })?;

        config.validate()?;
        config.resolve_paths();

        Ok(config)
    }

    /// Retorna o caminho padrão XDG para o arquivo de configuração.
    fn default_path() -> anyhow::Result<PathBuf> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            anyhow::anyhow!("Não foi possível determinar $XDG_CONFIG_HOME / ~/.config")
        })?;
        Ok(config_dir.join("amanuense").join("config.toml"))
    }

    /// Resolve `~` no caminho do modelo para o home directory real.
    fn resolve_paths(&mut self) {
        if self.model.path.starts_with('~')
            && let Some(home) = dirs::home_dir()
        {
            self.model.path = self.model.path.replacen('~', &home.to_string_lossy(), 1);
        }
    }

    /// Valida restrições e emite avisos para configurações subótimas.
    fn validate(&self) -> anyhow::Result<()> {
        // Segmento não pode ultrapassar 30s (limite interno do Whisper)
        if self.inference.segment_duration_secs > 30 {
            anyhow::bail!(
                "[inference] segment_duration_secs = {} é maior que 30s (limite do Whisper). \
                 Use um valor entre 15 e 28.",
                self.inference.segment_duration_secs
            );
        }

        // Sobreposição deve ser menor que a duração do segmento
        if self.inference.segment_overlap_secs >= self.inference.segment_duration_secs {
            anyhow::bail!(
                "[inference] segment_overlap_secs ({}) deve ser menor que \
                 segment_duration_secs ({}).",
                self.inference.segment_overlap_secs,
                self.inference.segment_duration_secs
            );
        }

        // Avisar se GPU desabilitada (incomum para este setup)
        if !self.model.use_gpu {
            warn!(
                "[model] use_gpu = false — a inferência será feita na CPU. \
                 Latência será significativamente maior."
            );
        }

        // Avisar sobre idioma automático (subótimo para pt-BR)
        if self.model.language == "pt" {
            warn!(
                "[model] language = \"pt\" — para a detecção automática de idioma \
                 é necessário alterar para o modo automático."
            );
        }

        // Taxa de amostragem fixada pelo Whisper
        if self.audio.sample_rate != 16000 {
            warn!(
                "[audio] sample_rate = {} — o Whisper requer 16000 Hz. \
                 Resample automático será aplicado, mas pode afetar a qualidade.",
                self.audio.sample_rate
            );
        }

        Ok(())
    }
}

impl IpcConfig {
    /// Retorna o caminho efetivo do socket, resolvendo o padrão XDG se necessário.
    pub fn resolved_socket_path(&self) -> anyhow::Result<PathBuf> {
        if !self.socket_path.is_empty() {
            return Ok(PathBuf::from(&self.socket_path));
        }

        // Padrão: /run/user/$UID/amanuense.sock
        let runtime_dir = dirs::runtime_dir().ok_or_else(|| {
            anyhow::anyhow!(
                "Não foi possível determinar $XDG_RUNTIME_DIR. \
                 Defina [ipc] socket_path no config.toml."
            )
        })?;

        Ok(runtime_dir.join("amanuense.sock"))
    }
}

impl InferenceConfig {
    /// Monta o prompt efetivo enviado ao Whisper concatenando system_prompt e initial_prompt.
    /// Retorna None se ambos estiverem vazios.
    pub fn effective_prompt(&self) -> Option<String> {
        let parts: Vec<&str> = [self.system_prompt.as_str(), self.initial_prompt.as_str()]
            .iter()
            .copied()
            .filter(|s| !s.trim().is_empty())
            .collect();

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }
}
