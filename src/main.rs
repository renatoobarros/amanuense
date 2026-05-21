//! main.rs — Entry point do amanuense.
//!
//! Subcomandos disponíveis:
//!
//!   amanuense daemon          Inicia o daemon (chamado pelo systemd)
//!   amanuense toggle          Envia toggle ao daemon em execução
//!   amanuense stop            Força parada da gravação
//!   amanuense status          Exibe o estado atual do daemon
//!   amanuense list-devices    Lista dispositivos de áudio disponíveis
//!
//! O mesmo binário serve como daemon e como cliente leve,
//! eliminando a necessidade de dois executáveis separados.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
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
    name = "amanuense",
    version,
    about = "Daemon de ditado por voz via Whisper — zero disco, VRAM residente",
    long_about = None,
)]
struct Cli {
    #[arg(short, long, value_name = "ARQUIVO")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Daemon,
    Toggle,
    Stop,
    Status,
    ListDevices,
}

// =============================================================================
// Entry point
// =============================================================================

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
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
    let ipc_config = Config::load_ipc_only(config_path.as_deref())?;
    let socket_path = ipc_config.resolved_socket_path()?;

    let response = daemon::ipc::send_command(socket_path.as_path(), command).await?;

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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .without_time()
        .compact()
        .init();
}
