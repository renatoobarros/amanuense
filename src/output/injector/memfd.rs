use std::os::unix::io::{AsFd, RawFd};

use wayland_client::protocol::wl_keyboard;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

/// Envia um keymap XKB ao compositor via file descriptor.
///
/// O protocolo zwp_virtual_keyboard exige que o keymap seja enviado como
/// um file descriptor somente-leitura contendo o texto XKB.
/// Usamos `memfd_create` (Linux) para criar um fd em memória sem tocar o disco.
///
/// O fd é mantido vivo até depois do flush do compositor para garantir
/// que a leitura foi concluída antes do fechamento.
pub(super) fn send_keymap_str(
    keyboard: &ZwpVirtualKeyboardV1,
    keymap_str: &str,
) -> anyhow::Result<()> {
    let bytes = keymap_str.as_bytes();
    let size = bytes.len();

    // Cria um arquivo em memória (sem disco) via memfd_create(2)
    let fd = create_memfd("xkb-keymap", size)?;

    // Escreve o keymap no fd diretamente (sem wrap em File)
    // SAFETY: fd é válido e owned por nós; write não fecha o fd
    let written = unsafe {
        let ptr = bytes.as_ptr() as *const std::ffi::c_void;
        let len = bytes.len() as libc::size_t;
        libc::write(fd, ptr, len)
    };
    if written < 0 as libc::ssize_t {
        unsafe { libc::close(fd) };
        anyhow::bail!(
            "Falha ao escrever keymap no memfd: {}",
            std::io::Error::last_os_error()
        );
    }

    // Envia ao compositor: formato XKB_V1, tamanho em bytes
    // O compositor faz dup() do fd internamente antes de retornarmos
    // Precisa criar o borrowing antes de passar para keymap para evitar temporary
    let fd_ref = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) };
    keyboard.keymap(
        wl_keyboard::KeymapFormat::XkbV1.into(),
        fd_ref.as_fd(),
        size as u32,
    );

    // Flush garante que o compositor recebeu o fd antes de fecharmos
    // SAFETY: fd ainda é válido; flush não fecha o fd
    unsafe {
        libc::fsync(fd);
    }

    // Agora é seguro fechar o fd — o compositor já fez a cópia
    unsafe { libc::close(fd) };

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
