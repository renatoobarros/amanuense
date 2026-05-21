use std::os::unix::io::{AsFd, FromRawFd, OwnedFd, RawFd};
use wayland_client::protocol::wl_keyboard;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

pub(super) fn create_and_send_keymap(
    keyboard: &ZwpVirtualKeyboardV1,
    keymap_str: &str,
) -> anyhow::Result<OwnedFd> {
    let mut bytes = keymap_str.as_bytes().to_vec();
    bytes.push(0); // C string terminada em null requerida pelo libxkbcommon

    let size = bytes.len();
    let fd = create_memfd("xkb-keymap", size)?;

    unsafe {
        let ptr = bytes.as_ptr() as *const std::ffi::c_void;
        libc::write(fd, ptr, size as libc::size_t);
    }

    // Transfere ownership: o fd agora será fechado automaticamente ao sair de escopo.
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
    let c_name = CString::new(name).unwrap();
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
