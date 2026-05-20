/// config.rs — Leitura e validação da configuração do usuário.
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{info, warn};

// =============================================================================
// Estruturas de configuração
// =============================================================================

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
    pub path: String,
    pub language: String,
    pub use_gpu: bool,
    pub gpu_device: i32,
    pub n_threads: i32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AudioConfig {
    pub device: String,
    pub sample_rate: u32,
    pub max_recording_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InferenceConfig {
    /// Intervalo de extração de blocos de áudio e injeção de texto (em ms)
    pub stream_step_ms: u32,

    /// Tamanho da janela contínua de áudio fornecida à GPU (em segundos)
    pub stream_window_secs: u32,

    pub initial_prompt: String,
    pub system_prompt: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OutputConfig {
    pub primary_selection: bool,
    pub newline_on_finish: bool,
    pub typing_delay_ms: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NotificationConfig {
    pub notify_on_start: bool,
    pub start_message: String,
    pub notify_on_finish: bool,
    pub finish_message: String,
    pub start_timeout_ms: u32,
    pub finish_timeout_ms: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IpcConfig {
    pub socket_path: String,
}

// =============================================================================
// Implementações
// =============================================================================

impl Config {
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

    fn default_path() -> anyhow::Result<PathBuf> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            anyhow::anyhow!("Não foi possível determinar $XDG_CONFIG_HOME / ~/.config")
        })?;
        Ok(config_dir.join("amanuense").join("config.toml"))
    }

    fn resolve_paths(&mut self) {
        if self.model.path.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                let without_tilde = &self.model.path[2..];
                self.model.path = home.join(without_tilde).to_string_lossy().to_string();
            }
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.inference.stream_window_secs > 30 {
            anyhow::bail!(
                "[inference] stream_window_secs = {} é maior que 30s (limite arquitetural do Whisper).",
                self.inference.stream_window_secs
            );
        }

        if self.inference.stream_step_ms < 100 {
            warn!(
                "[inference] stream_step_ms = {}ms é muito baixo e pode causar engasgos na GPU.",
                self.inference.stream_step_ms
            );
        }

        if !self.model.use_gpu {
            warn!(
                "[model] use_gpu = false — inferência em CPU causará extrema latência no streaming."
            );
        }

        if self.model.language == "pt" {
            info!("[model] language = \"pt\" — idioma Português do Brasil ativado.");
        } else if self.model.language == "auto" {
            warn!(
                "[model] language = \"auto\" — detecção automática adiciona latência ao streaming. Considere fixar."
            );
        }

        Ok(())
    }
}

impl IpcConfig {
    pub fn resolved_socket_path(&self) -> anyhow::Result<PathBuf> {
        if !self.socket_path.is_empty() {
            return Ok(PathBuf::from(&self.socket_path));
        }

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
