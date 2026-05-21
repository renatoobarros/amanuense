use std::os::unix::io::{AsFd, FromRawFd, OwnedFd, RawFd};
use wayland_client::protocol::wl_keyboard;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

pub(super) fn create_and_send_keymap(
    keyboard: &ZwpVirtualKeyboardV1,
    keymap_str: &str,
) -> anyhow::Result<OwnedFd> {
    let mut bytes = keymap_str.as_bytes().to_vec();
    bytes.push(0);

    let size = bytes.len();
    let fd = create_memfd("xkb-keymap", size)?;

    unsafe {
        let ptr = bytes.as_ptr() as *const std::ffi::c_void;
        let mut written = 0isize;

        while written < size as isize {
            let ret = libc::write(fd, ptr.offset(written), size - written as usize);
            if ret < 0 {
                libc::close(fd);
                anyhow::bail!(
                    "Falha ao escrever keymap no memfd: {}",
                    std::io::Error::last_os_error()
                );
            }
            written += ret;
        }
    }

    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };

    keyboard.keymap(
        wl_keyboard::KeymapFormat::XkbV1.into(),
        owned_fd.as_fd(),
        size as u32,
    );

    Ok(owned_fd)
}

fn create_memfd(name: &str, size: usize) -> anyhow::Result<RawFd> {
    use std::ffi::CString;
    // Ponto 4: Mapeamento seguro do erro no lugar do .unwrap()
    let c_name =
        CString::new(name).map_err(|e| anyhow::anyhow!("Nome de memfd inválido: {}", e))?;

    let fd = unsafe { libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC) };

    if fd < 0 {
        anyhow::bail!("memfd_create falhou: {}", std::io::Error::last_os_error());
    }

    let ret = unsafe { libc::ftruncate(fd, size as libc::off_t) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("ftruncate falhou: {}", std::io::Error::last_os_error());
    }

    Ok(fd)
}
