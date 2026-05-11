/// main.rs — Entry point do whisper-dictate.
///
/// Subcomandos disponíveis:
///
///   whisper-dictate daemon          Inicia o daemon (chamado pelo systemd)
///   whisper-dictate toggle          Envia toggle ao daemon em execução
///   whisper-dictate stop            Força parada da gravação
///   whisper-dictate status          Exibe o estado atual do daemon
///   whisper-dictate list-devices    Lista dispositivos de áudio disponíveis
///
/// O mesmo binário serve como daemon e como cliente leve,
/// eliminando a necessidade de dois executáveis separados.
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod config;
mod daemon;
mod output;

use config::Config;

// =============================================================================
// CLI
// =============================================================================

#[derive(Parser)]
#[command(
    name = "whisper-dictate",
    version,
    about = "Daemon de ditado por voz via Whisper — zero disco, VRAM residente",
    long_about = None,
)]
struct Cli {
    /// Caminho alternativo para o arquivo de configuração.
    /// Padrão: ~/.config/whisper-dictate/config.toml
    #[arg(short, long, value_name = "ARQUIVO")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inicia o daemon (carrega o modelo na GPU e aguarda comandos).
    /// Normalmente chamado pelo systemd, não pelo usuário diretamente.
    Daemon,

    /// Alterna entre iniciar e parar a gravação.
    /// Use este subcomando no atalho do Niri.
    Toggle,

    /// Força a parada da gravação (se estiver gravando).
    Stop,

    /// Exibe o estado atual do daemon (idle | recording | processing).
    Status,

    /// Lista os dispositivos de entrada de áudio disponíveis no sistema.
    ListDevices,
}

// =============================================================================
// Entry point
// =============================================================================

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Inicializa o sistema de logging.
    // O nível pode ser sobrescrito via RUST_LOG (ex: RUST_LOG=debug).
    init_logging();

    let result = match cli.command {
        Command::Daemon => run_daemon(cli.config).await,
        Command::Toggle => run_client(cli.config, "toggle").await,
        Command::Stop => run_client(cli.config, "stop").await,
        Command::Status => run_client(cli.config, "status").await,
        Command::ListDevices => run_list_devices(),
    };

    if let Err(e) = result {
        eprintln!("Erro: {:#}", e);
        std::process::exit(1);
    }
}

// =============================================================================
// Modo daemon
// =============================================================================

async fn run_daemon(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = Config::load(config_path.as_deref())?;
    daemon::run(config).await
}

// =============================================================================
// Modo cliente (toggle / stop / status)
// =============================================================================

async fn run_client(config_path: Option<PathBuf>, command: &str) -> anyhow::Result<()> {
    let config = Config::load(config_path.as_deref())?;
    let socket_path = config.ipc.resolved_socket_path()?;

    let response = daemon::ipc::send_command(&socket_path, command).await?;

    // Exibe a resposta apenas para `status` — os outros comandos são silenciosos
    // para não interferir com o fluxo de trabalho do usuário
    match command {
        "status" => println!("{}", response),
        _ => {
            if response.starts_with("err") {
                anyhow::bail!("Daemon reportou erro: {}", response);
            }
        }
    }

    Ok(())
}

// =============================================================================
// Listagem de dispositivos
// =============================================================================

fn run_list_devices() -> anyhow::Result<()> {
    let devices = daemon::audio::AudioCapture::list_devices()?;

    if devices.is_empty() {
        println!("Nenhum dispositivo de entrada encontrado.");
    } else {
        println!("Dispositivos de entrada disponíveis:");
        for (i, name) in devices.iter().enumerate() {
            println!("  [{}] {}", i, name);
        }
        println!("\nUse o nome exato no campo [audio] device do config.toml.");
    }

    Ok(())
}

// =============================================================================
// Logging
// =============================================================================

fn init_logging() {
    // Nível padrão: warn (silencioso em produção)
    // Sobrescrever com: RUST_LOG=whisper_dictate=info ou RUST_LOG=debug
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)       // Remove o nome do módulo do log (mais limpo)
        .with_thread_ids(false)
        .without_time()           // systemd já adiciona timestamp nas entradas de log
        .compact()
        .init();
}
