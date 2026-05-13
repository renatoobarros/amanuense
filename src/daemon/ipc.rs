/// ipc.rs — Servidor Unix Domain Socket.
///
/// Protocolo de texto simples, uma mensagem por linha:
///
///   Cliente → Daemon   |   Daemon → Cliente
///   -------------------|--------------------
///   "toggle\n"         |   "ok\n"
///   "status\n"         |   "idle\n" | "recording\n" | "processing\n"
///   "stop\n"           |   "ok\n"   (força parada se estiver gravando)
///
/// Texto plano é usado por simplicidade e debug — não requer serialização
/// e pode ser testado manualmente com `echo "toggle" | nc -U /path/to/sock`.
/// Nenhum dado de áudio ou texto transcrito trafega pelo socket —
/// apenas sinais de controle. Conformidade LGPD por design.
use std::path::PathBuf;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

// =============================================================================
// Tipos públicos
// =============================================================================

/// Comandos que o servidor IPC pode despachar para o loop principal do daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    /// Alterna entre gravar e parar (comportamento toggle)
    Toggle,
    /// Força a parada da gravação independente do estado atual
    Stop,
}

/// Estado atual do daemon, reportado em resposta ao comando "status".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    /// Modelo carregado, aguardando comando
    Idle,
    /// Microfone aberto, coletando áudio
    Recording,
    /// Gravação encerrada, inferência final em andamento
    Processing,
}

impl DaemonState {
    fn as_str(&self) -> &'static str {
        match self {
            DaemonState::Idle => "idle",
            DaemonState::Recording => "recording",
            DaemonState::Processing => "processing",
        }
    }
}

// =============================================================================
// Servidor IPC
// =============================================================================

/// Inicia o servidor Unix Domain Socket em uma task Tokio dedicada.
///
/// Parâmetros:
/// - `socket_path`: caminho onde o socket será criado
/// - `cmd_tx`: canal para enviar IpcCommand ao loop principal do daemon
/// - `state_rx`: receptor para consultar o estado atual do daemon
///
/// Retorna um JoinHandle da task do servidor.
pub async fn start_server(
    socket_path: PathBuf,
    cmd_tx: mpsc::Sender<IpcCommand>,
    state_rx: tokio::sync::watch::Receiver<DaemonState>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    // Remove socket antigo se existir (crash anterior, por exemplo)
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
        warn!("Socket anterior removido: {}", socket_path.display());
    }

    let listener = UnixListener::bind(&socket_path).map_err(|e| {
        anyhow::anyhow!(
            "Não foi possível criar socket em '{}': {}",
            socket_path.display(),
            e
        )
    })?;

    info!("IPC socket ouvindo em: {}", socket_path.display());

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let tx = cmd_tx.clone();
                    let rx = state_rx.clone();
                    // Spawn com verificação de pânico
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, tx, rx).await {
                            error!("Conexão IPC encerrada com erro: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Erro ao aceitar conexão IPC: {}", e);
                }
            }
        }
    });

    Ok(handle)
}

/// Envia um comando ao daemon já em execução conectando-se ao socket.
/// Usado pelo subcomando `toggle` (modo cliente).
pub async fn send_command(socket_path: &PathBuf, command: &str) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|_| {
        anyhow::anyhow!(
            "Não foi possível conectar ao daemon em '{}'. \
             Verifique se o daemon está rodando: systemctl --user status amanuense",
            socket_path.display()
        )
    })?;

    // Envia comando
    stream
        .write_all(format!("{}\n", command).as_bytes())
        .await?;

    // Lê resposta
    let mut reader = BufReader::new(&mut stream);
    let mut response = String::new();
    reader.read_line(&mut response).await?;

    Ok(response.trim().to_string())
}

// =============================================================================
// Handlers internos
// =============================================================================

/// Trata uma conexão individual de cliente.
/// Cada conexão é processada em sua própria task Tokio.
async fn handle_connection(
    stream: UnixStream,
    cmd_tx: mpsc::Sender<IpcCommand>,
    state_rx: tokio::sync::watch::Receiver<DaemonState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();

    match reader.read_line(&mut line).await {
        Ok(0) => {
            // Conexão fechada sem enviar dados
            debug!("Conexão IPC encerrada sem comando.");
            return Ok(());
        }
        Err(e) => {
            error!("Erro ao ler comando IPC: {}", e);
            return Ok(());
        }
        Ok(_) => {}
    }

    let command = line.trim();
    debug!("Comando IPC recebido: '{}'", command);

    let response = match command {
        "toggle" => match cmd_tx.send(IpcCommand::Toggle).await {
            Ok(_) => "ok".to_string(),
            Err(_) => "err: daemon não está respondendo".to_string(),
        },
        "stop" => match cmd_tx.send(IpcCommand::Stop).await {
            Ok(_) => "ok".to_string(),
            Err(_) => "err: daemon não está respondendo".to_string(),
        },
        "status" => {
            let state = *state_rx.borrow();
            state.as_str().to_string()
        }
        other => {
            warn!("Comando IPC desconhecido: '{}'", other);
            format!("err: comando desconhecido '{}'", other)
        }
    };

    if let Err(e) = writer_half
        .write_all(format!("{}\n", response).as_bytes())
        .await
    {
        error!("Erro ao enviar resposta IPC: {}", e);
    }

    Ok(())
}
