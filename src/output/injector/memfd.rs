use std::io::Write;
use std::os::unix::io::{AsFd, FromRawFd, RawFd};

use wayland_client::protocol::wl_keyboard;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

/// Envia um keymap XKB ao compositor via file descriptor.
///
/// O protocolo zwp_virtual_keyboard exige que o keymap seja enviado como
/// um file descriptor somente-leitura contendo o texto XKB.
/// Usamos `memfd_create` (Linux) para criar um fd em memória sem tocar o disco.
pub(super) fn send_keymap_str(
    keyboard: &ZwpVirtualKeyboardV1,
    keymap_str: &str,
) -> anyhow::Result<()> {
    let bytes = keymap_str.as_bytes();
    let size = bytes.len();

    // Cria um arquivo em memória (sem disco) via memfd_create(2)
    let fd = create_memfd("xkb-keymap", size)?;

    // Escreve o keymap no fd
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    {
        let mut writer = file;
        writer
            .write_all(bytes)
            .map_err(|e| anyhow::anyhow!("Falha ao escrever keymap no memfd: {}", e))?;

        // Enviar ao compositor: formato XKB_V1, tamanho em bytes
        keyboard.keymap(
            wl_keyboard::KeymapFormat::XkbV1.into(),
            writer.as_fd(),
            size as u32,
        );
        // `writer` fecha o fd ao sair de escopo (ok: o compositor já recebeu o fd).
    }

    Ok(())
}

/// Cria um file descriptor anônimo em memória via `memfd_create(2)`.
/// Não cria nenhum arquivo em disco — LGPD safe.
fn create_memfd(name: &str, size: usize) -> anyhow::Result<RawFd> {
    use std::ffi::CString;

    let c_name = CString::new(name).unwrap();

    // SAFETY: chamada syscall direta; name é um CString válido
    let fd = unsafe { libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC) };

    if fd < 0 {
        anyhow::bail!("memfd_create falhou: {}", std::io::Error::last_os_error());
    }

    // Pré-aloca o tamanho necessário
    let ret = unsafe { libc::ftruncate(fd, size as libc::off_t) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("ftruncate falhou: {}", std::io::Error::last_os_error());
    }

    Ok(fd)
}
