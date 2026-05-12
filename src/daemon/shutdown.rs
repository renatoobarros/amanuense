use tracing::info;

pub(super) async fn setup_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm =
        signal(SignalKind::terminate()).expect("Falha ao registrar handler de SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("Falha ao registrar handler de SIGINT");

    tokio::select! {
        _ = sigterm.recv() => info!("SIGTERM recebido."),
        _ = sigint.recv()  => info!("SIGINT recebido."),
    }
}
