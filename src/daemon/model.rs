use crate::config::ModelConfig;
use std::sync::Mutex;
use tracing::info;
use whisper_rs::{WhisperContext, WhisperContextParameters};

pub struct WhisperModel {
    ctx: Mutex<WhisperContext>,
}

unsafe impl Send for WhisperModel {}
unsafe impl Sync for WhisperModel {}

impl WhisperModel {
    pub fn load(config: &ModelConfig) -> anyhow::Result<Self> {
        let path = std::path::Path::new(&config.path);
        if !path.exists() {
            anyhow::bail!("Arquivo do modelo não encontrado: '{}'", config.path);
        }

        let mut params = WhisperContextParameters::default();
        params.use_gpu(config.use_gpu);
        params.gpu_device(config.gpu_device);

        info!(
            "Carregando modelo '{}' (GPU={})",
            path.display(),
            config.use_gpu
        );

        let ctx = WhisperContext::new_with_params(&config.path, params)
            .map_err(|e| anyhow::anyhow!("Falha ao carregar modelo Whisper: {:?}", e))?;

        info!("Modelo carregado com sucesso.");
        Ok(Self {
            ctx: Mutex::new(ctx),
        })
    }

    pub fn create_state(&self) -> anyhow::Result<whisper_rs::WhisperState> {
        let ctx = self.ctx.lock().unwrap();
        ctx.create_state()
            .map_err(|e| anyhow::anyhow!("Falha ao criar estado: {:?}", e))
    }
}
