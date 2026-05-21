/// ipc.rs — Servidor Unix Domain Socket.
use std::path::{Path, PathBuf};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::mpsc,
    time::{Duration, timeout},
};
use tracing::{debug, error, info, warn};

// =============================================================================
// Tipos públicos
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    Toggle,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    Idle,
    Recording,
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

pub async fn start_server(
    socket_path: PathBuf,
    cmd_tx: mpsc::Sender<IpcCommand>,
    state_rx: tokio::sync::watch::Receiver<DaemonState>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    if socket_path.exists() {
        if UnixStream::connect(&socket_path).await.is_ok() {
            anyhow::bail!(
                "Um daemon do amanuense já está em execução (socket respondendo em {}). \
                 Use `systemctl --user restart amanuense` se precisar reiniciar.",
                socket_path.display()
            );
        } else {
            std::fs::remove_file(&socket_path)?;
            warn!("Socket órfão anterior removido: {}", socket_path.display());
        }
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

pub async fn send_command(socket_path: &Path, command: &str) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|_| {
        anyhow::anyhow!(
            "Não foi possível conectar ao daemon em '{}'. \
             Verifique se o daemon está rodando: systemctl --user status amanuense",
            socket_path.display()
        )
    })?;

    stream
        .write_all(format!("{}\n", command).as_bytes())
        .await?;

    let mut reader = BufReader::new(&mut stream);
    let mut response = String::new();
    reader.read_line(&mut response).await?;

    Ok(response.trim().to_string())
}

// =============================================================================
// Handlers internos
// =============================================================================

async fn handle_connection(
    stream: UnixStream,
    cmd_tx: mpsc::Sender<IpcCommand>,
    state_rx: tokio::sync::watch::Receiver<DaemonState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();

    match timeout(Duration::from_secs(5), reader.read_line(&mut line)).await {
        Ok(Ok(0)) => {
            debug!("Conexão IPC encerrada sem comando.");
            return Ok(());
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            error!("Erro ao ler comando IPC: {}", e);
            return Ok(());
        }
        Err(_) => {
            warn!("Timeout na leitura do comando IPC.");
            return Ok(());
        }
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
