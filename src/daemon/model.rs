/// model.rs — Carregamento e gerenciamento do modelo Whisper.
///
/// O modelo é carregado uma única vez na inicialização do daemon e
/// permanece residente na VRAM enquanto o serviço estiver ativo.
///
/// `WhisperModel` é `Send + Sync` via `Mutex<WhisperContext>`, permitindo
/// que seja compartilhado via `Arc<WhisperModel>` entre tasks Tokio e
/// threads de inferência (`spawn_blocking`).
use std::sync::Mutex;

use whisper_rs::{WhisperContext, WhisperContextParameters};
use tracing::info;

use crate::config::ModelConfig;

// =============================================================================
// Struct pública
// =============================================================================

/// Wrapper thread-safe em torno do `WhisperContext`.
///
/// O `Mutex` garante acesso exclusivo durante a inferência.
/// Na prática, a máquina de estados do daemon nunca inicia duas
/// inferências simultaneamente — o mutex é uma proteção defensiva.
pub struct WhisperModel {
    ctx: Mutex<WhisperContext>,
}

// SAFETY: WhisperContext contém um ponteiro opaco para o contexto C
// do whisper.cpp. O acesso é serializado pelo Mutex, tornando o uso
// thread-safe. Necessário para colocar WhisperModel em Arc<>.
unsafe impl Send for WhisperModel {}
unsafe impl Sync for WhisperModel {}

// =============================================================================
// Implementação
// =============================================================================

impl WhisperModel {
    /// Carrega o modelo GGML do disco para a VRAM (ou RAM se use_gpu=false).
    ///
    /// Esta operação é custosa (~1-5s dependendo do modelo e da GPU).
    /// Deve ser chamada uma única vez na inicialização do daemon.
    pub fn load(config: &ModelConfig) -> anyhow::Result<Self> {
        let path = std::path::Path::new(&config.path);

        if !path.exists() {
            anyhow::bail!(
                "Arquivo do modelo não encontrado: '{}'\n\
                 Verifique o campo [model] path no config.toml.",
                config.path
            );
        }

        // Parâmetros de carregamento do contexto
        let mut params = WhisperContextParameters::default();
        params.use_gpu(config.use_gpu);
        params.gpu_device(config.gpu_device);

        info!(
            "Carregando modelo '{}' (GPU={}, device={})",
            path.display(),
            config.use_gpu,
            config.gpu_device
        );

        let ctx = WhisperContext::new_with_params(&config.path, params).map_err(|e| {
            anyhow::anyhow!(
                "Falha ao carregar modelo Whisper em '{}': {:?}\n\
                 Verifique se o arquivo está íntegro e se o suporte a CUDA \
                 foi compilado (WHISPER_CUDA=1).",
                config.path,
                e
            )
        })?;

        info!("Modelo carregado com sucesso.");

        Ok(Self {
            ctx: Mutex::new(ctx),
        })
    }

    /// Cria um novo estado de inferência a partir do contexto carregado.
    ///
    /// O estado encapsula os tensores intermediários de uma sessão de
    /// inferência. Criar um novo a cada transcrição garante isolamento
    /// entre sessões sem recarregar o modelo.
    pub fn create_state(&self) -> anyhow::Result<whisper_rs::WhisperState> {
        let ctx = self.ctx.lock().map_err(|_| {
            anyhow::anyhow!("Mutex do modelo Whisper envenenado — reinicie o daemon.")
        })?;

        ctx.create_state().map_err(|e| {
            anyhow::anyhow!("Falha ao criar estado de inferência: {:?}", e)
        })
    }
}
