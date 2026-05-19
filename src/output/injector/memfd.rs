use std::os::unix::io::{AsFd, RawFd};

use wayland_client::protocol::wl_keyboard;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

pub(super) fn send_keymap_str(
    keyboard: &ZwpVirtualKeyboardV1,
    keymap_str: &str,
) -> anyhow::Result<()> {
    // Terminação nula obrigatória para a biblioteca libxkbcommon em C no compositor
    let mut bytes = keymap_str.as_bytes().to_vec();
    bytes.push(0);

    let size = bytes.len();
    let fd = create_memfd("xkb-keymap", size)?;

    let written = unsafe {
        let ptr = bytes.as_ptr() as *const std::ffi::c_void;
        libc::write(fd, ptr, size as libc::size_t)
    };

    if written < 0 as libc::ssize_t {
        unsafe { libc::close(fd) };
        anyhow::bail!(
            "Falha ao escrever keymap no memfd: {}",
            std::io::Error::last_os_error()
        );
    }

    let fd_ref = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) };
    keyboard.keymap(
        wl_keyboard::KeymapFormat::XkbV1.into(),
        fd_ref.as_fd(),
        size as u32,
    );

    unsafe {
        libc::fsync(fd);
        libc::close(fd);
    }

    Ok(())
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
