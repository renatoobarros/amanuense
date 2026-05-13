pub(super) fn notify_start(
    cfg: &crate::config::NotificationConfig,
) -> Option<notify_rust::NotificationHandle> {
    use notify_rust::Notification;

    let timeout = if cfg.start_timeout_ms == 0 {
        notify_rust::Timeout::Never
    } else {
        notify_rust::Timeout::Milliseconds(cfg.start_timeout_ms)
    };

    match Notification::new()
        .summary(&cfg.start_message)
        .timeout(timeout)
        .show()
    {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!("Falha ao exibir notificação de início: {}", e);
            None
        }
    }
}

pub(super) fn notify_finish(cfg: &crate::config::NotificationConfig, transcribed_text: &str) {
    use notify_rust::Notification;

    // Exibe um preview do texto transcrito no corpo da notificação
    let mut chars = transcribed_text.chars();
    let preview: String = chars.by_ref().take(120).collect();
    let preview = if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    };

    if let Err(e) = Notification::new()
        .summary(&cfg.finish_message)
        .body(&preview)
        .timeout(notify_rust::Timeout::Milliseconds(cfg.finish_timeout_ms))
        .show()
    {
        tracing::warn!("Falha ao exibir notificação de conclusão: {}", e);
    }
}

/// Exibe notificação de erro quando a captura de áudio falha.
/// O parâmetro cfg está reservado para customização futura (ícone, som, etc.).
pub(super) fn notify_error(_cfg: &crate::config::NotificationConfig, error_msg: &str) {
    use notify_rust::Notification;

    let _ = Notification::new()
        .summary("Erro na captura de áudio")
        .body(error_msg)
        .timeout(notify_rust::Timeout::Milliseconds(5000))
        .show();
}
