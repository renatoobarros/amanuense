use std::process::Command;
use tracing::warn;

pub fn set_primary_selection(text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let status = Command::new("wl-copy").arg("--primary").arg(text).status();

    match status {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => {
            warn!("wl-copy retornou código de erro: {}", st);
            Err(anyhow::anyhow!(
                "Erro ao definir seleção primária via wl-copy"
            ))
        }
        Err(e) => {
            warn!(
                "Falha ao executar wl-copy: {}. O wl-clipboard está instalado?",
                e
            );
            Err(anyhow::anyhow!(
                "Utilitário wl-copy não encontrado ou falhou"
            ))
        }
    }
}
