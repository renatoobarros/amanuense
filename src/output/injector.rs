/// injector.rs — Injeção de texto via `zwp_virtual_keyboard_v1`.
///
/// Protocolo: `virtual-keyboard-unstable-v1`
/// Suportado por: wlroots, niri, sway, river e demais compositors wlroots.
///
/// Funcionamento:
///   O protocolo simula um teclado físico no nível do compositor.
///   Para digitar texto arbitrário (incluindo Unicode completo), usamos
///   a seguinte estratégia:
///
///   1. Criamos um keymap XKB mínimo com uma única tecla (`KEY_A`, código 30)
///      mapeada dinamicamente para o codepoint Unicode desejado.
///   2. Para cada caractere do texto:
///      a. Geramos um keymap XKB temporário com aquele caractere no slot da tecla
///      b. Enviamos `keymap()` ao compositor com o novo mapeamento
///      c. Enviamos `key(press)` + `key(release)` para a tecla 30
///   3. Caracteres especiais (newline, tab) são mapeados para keysyms padrão.
///
///   Esta abordagem suporta qualquer codepoint Unicode sem depender de
///   xdotool, ydotool, wtype ou qualquer ferramenta externa.
///
/// Trade-off de performance:
///   Gerar um keymap por caractere tem overhead. Para textos longos (centenas
///   de caracteres), isso é perceptível mas aceitável — a alternativa seria
///   implementar um keymap pré-compilado com todos os caracteres pt-BR, o que
///   adicionaria complexidade significativa sem ganho prático para este uso.
///
/// LGPD: o texto trafega apenas em memória e via socket Wayland local.
use std::io::Write;
use std::os::unix::io::FromRawFd;

use wayland_client::{
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::unstable::virtual_keyboard::v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use tracing::{debug, warn};

// Código de tecla físico usado como "slot" para injeção de caracteres.
// KEY_A (30) é arbitrário — qualquer tecla serve, usamos sempre a mesma.
const INJECT_KEY_CODE: u32 = 30;

// Tempo simulado entre press e release (em ms). O Wayland usa timestamps
// relativos — usamos valores incrementais simples.
const KEY_PRESS_TIME: u32 = 100;
const KEY_RELEASE_TIME: u32 = 200;

// =============================================================================
// Struct pública
// =============================================================================

/// Injetor de texto via teclado virtual Wayland.
///
/// Conecta ao compositor na criação e mantém a conexão ativa
/// para toda a vida do daemon.
pub struct TextInjector {
    conn: Connection,
    event_queue: EventQueue<InjectorState>,
    state: InjectorState,
    time_counter: u32,
}

impl TextInjector {
    /// Conecta ao compositor Wayland e inicializa o teclado virtual.
    pub fn new() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| anyhow::anyhow!("Falha ao conectar ao Wayland: {}", e))?;

        let mut event_queue: EventQueue<InjectorState> = conn.new_event_queue();
        let qh = event_queue.handle();

        let display = conn.display();
        display.get_registry(&qh, ());

        let mut state = InjectorState {
            seat: None,
            manager: None,
            keyboard: None,
            qh: qh.clone(),
        };

        // Obtém globals do compositor
        event_queue.roundtrip(&mut state)?;

        // Valida presença do protocolo virtual-keyboard
        let manager = state.manager.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "zwp_virtual_keyboard_manager_v1 não encontrado.\n\
                 O compositor precisa suportar virtual-keyboard-unstable-v1.\n\
                 No niri, verifique se está em versão recente (25.x+)."
            )
        })?;

        let seat = state.seat.as_ref().ok_or_else(|| {
            anyhow::anyhow!("wl_seat não encontrado no compositor.")
        })?;

        // Cria o objeto de teclado virtual
        let keyboard = manager.create_virtual_keyboard(seat, &qh, ());
        state.keyboard = Some(keyboard);

        // Envia um keymap inicial vazio para satisfazer o protocolo.
        // Alguns compositors requerem pelo menos um keymap antes de aceitar key events.
        send_initial_keymap(&state)?;
        event_queue.flush()?;

        debug!("Teclado virtual Wayland inicializado.");

        Ok(Self {
            conn,
            event_queue,
            state,
            time_counter: KEY_RELEASE_TIME,
        })
    }

    /// Injeta o texto no campo com foco ativo no compositor.
    ///
    /// Cada caractere é digitado individualmente via keymap dinâmico.
    /// Newlines e tabs são enviados como keysyms padrão.
    pub fn type_text(&self, text: &str) -> anyhow::Result<()> {
        let keyboard = self.state.keyboard.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Teclado virtual não inicializado.")
        })?;

        debug!("Injetando {} caracteres via teclado virtual.", text.chars().count());

        // Contador de tempo incremental (evita timestamps duplicados)
        let mut t = self.time_counter;

        for ch in text.chars() {
            match ch {
                '\n' => {
                    // Return/Enter: keysym padrão, sem necessidade de keymap dinâmico
                    inject_keysym(keyboard, 0xff0d, t, t + 10)?;
                }
                '\t' => {
                    // Tab: keysym padrão
                    inject_keysym(keyboard, 0xff09, t, t + 10)?;
                }
                ' ' => {
                    // Espaço: keysym padrão
                    inject_keysym(keyboard, 0x0020, t, t + 10)?;
                }
                c => {
                    // Caractere Unicode arbitrário: keymap dinâmico
                    inject_unicode_char(keyboard, c, t, t + 10)?;
                }
            }

            t += 20; // Incrementa o timestamp para o próximo caractere
        }

        // Flush: envia todos os eventos ao compositor de uma vez
        self.conn.flush()
            .map_err(|e| anyhow::anyhow!("Falha ao enviar eventos de teclado: {}", e))?;

        // Processa respostas do compositor (necessário para manter o event queue limpo)
        // Usamos dispatch_pending em vez de blocking_dispatch para não bloquear
        // (não esperamos resposta — virtual keyboard é fire-and-forget)
        let mut state = InjectorState {
            seat: self.state.seat.clone(),
            manager: self.state.manager.clone(),
            keyboard: self.state.keyboard.clone(),
            qh: self.state.qh.clone(),
        };
        // Descarta eventos pendentes sem bloquear
        let _ = self.event_queue.dispatch_pending(&mut state);

        debug!("Injeção concluída.");
        Ok(())
    }
}

// =============================================================================
// Injeção de keysym padrão (teclas especiais)
// =============================================================================

/// Injeta um keysym XKB padrão (press + release).
fn inject_keysym(
    keyboard: &ZwpVirtualKeyboardV1,
    keysym: u32,
    press_time: u32,
    release_time: u32,
) -> anyhow::Result<()> {
    // Para keysyms simples, usamos um keymap mínimo com a tecla mapeada
    let keymap_str = build_keysym_keymap(keysym);
    send_keymap_str(keyboard, &keymap_str)?;

    // Press
    keyboard.key(press_time, INJECT_KEY_CODE, zwp_virtual_keyboard_v1::KeyState::Pressed);
    // Release
    keyboard.key(release_time, INJECT_KEY_CODE, zwp_virtual_keyboard_v1::KeyState::Released);

    Ok(())
}

// =============================================================================
// Injeção de caractere Unicode via keymap dinâmico
// =============================================================================

/// Injeta um caractere Unicode arbitrário criando um keymap XKB temporário.
fn inject_unicode_char(
    keyboard: &ZwpVirtualKeyboardV1,
    ch: char,
    press_time: u32,
    release_time: u32,
) -> anyhow::Result<()> {
    let keymap_str = build_unicode_keymap(ch);
    send_keymap_str(keyboard, &keymap_str)?;

    keyboard.key(press_time, INJECT_KEY_CODE, zwp_virtual_keyboard_v1::KeyState::Pressed);
    keyboard.key(release_time, INJECT_KEY_CODE, zwp_virtual_keyboard_v1::KeyState::Released);

    Ok(())
}

// =============================================================================
// Geração de keymap XKB mínimo
// =============================================================================

/// Gera um keymap XKB mínimo mapeando INJECT_KEY_CODE para um keysym padrão.
fn build_keysym_keymap(keysym: u32) -> String {
    format!(
        r#"xkb_keymap {{
    xkb_keycodes "inject" {{
        minimum = 8;
        maximum = 255;
        <INJECT> = {key_code};
    }};
    xkb_types "inject" {{
        include "complete"
    }};
    xkb_compatibility "inject" {{
        include "complete"
    }};
    xkb_symbols "inject" {{
        key <INJECT> {{ [ {keysym_hex} ] }};
    }};
}};"#,
        key_code = INJECT_KEY_CODE + 8, // XKB usa offset +8 em relação ao evdev
        keysym_hex = format!("0x{:08x}", keysym),
    )
}

/// Gera um keymap XKB mínimo mapeando INJECT_KEY_CODE para um codepoint Unicode.
///
/// XKB suporta keysyms Unicode diretamente no formato `U<codepoint_hex>`.
/// Ex: 'ã' (U+00E3) → `U00E3`
fn build_unicode_keymap(ch: char) -> String {
    let codepoint = ch as u32;

    // XKB keysym para Unicode: 0x01000000 + codepoint (para codepoints > 0x100)
    // Para ASCII e Latin-1 (≤ 0xFF), o keysym é igual ao codepoint diretamente.
    let keysym = if codepoint <= 0x00ff {
        codepoint
    } else {
        0x0100_0000 + codepoint
    };

    build_keysym_keymap(keysym)
}

// =============================================================================
// Envio do keymap ao compositor via memfd / pipe
// =============================================================================

/// Envia um keymap XKB ao compositor via file descriptor.
///
/// O protocolo zwp_virtual_keyboard exige que o keymap seja enviado como
/// um file descriptor somente-leitura contendo o texto XKB.
/// Usamos `memfd_create` (Linux) para criar um fd em memória sem tocar o disco.
fn send_keymap_str(keyboard: &ZwpVirtualKeyboardV1, keymap_str: &str) -> anyhow::Result<()> {
    let bytes = keymap_str.as_bytes();
    let size = bytes.len();

    // Cria um arquivo em memória (sem disco) via memfd_create(2)
    let fd = create_memfd("xkb-keymap", size)?;

    // Escreve o keymap no fd
    {
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(bytes)
            .map_err(|e| anyhow::anyhow!("Falha ao escrever keymap no memfd: {}", e))?;
        // file é dropped aqui mas NÃO fechamos o fd — será fechado pelo compositor
        std::mem::forget(file);
    }

    // Enviar ao compositor: formato XKB_V1, tamanho em bytes
    keyboard.keymap(
        zwp_virtual_keyboard_v1::KeymapFormat::XkbV1,
        unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) },
        size as u32,
    );

    Ok(())
}

/// Cria um file descriptor anônimo em memória via `memfd_create(2)`.
/// Não cria nenhum arquivo em disco — LGPD safe.
fn create_memfd(name: &str, size: usize) -> anyhow::Result<std::os::unix::io::RawFd> {
    use std::ffi::CString;

    let c_name = CString::new(name).unwrap();

    // SAFETY: chamada syscall direta; name é um CString válido
    let fd = unsafe {
        libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC)
    };

    if fd < 0 {
        anyhow::bail!(
            "memfd_create falhou: {}",
            std::io::Error::last_os_error()
        );
    }

    // Pré-aloca o tamanho necessário
    let ret = unsafe { libc::ftruncate(fd, size as libc::off_t) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!(
            "ftruncate falhou: {}",
            std::io::Error::last_os_error()
        );
    }

    Ok(fd)
}

// =============================================================================
// Keymap inicial vazio
// =============================================================================

/// Envia um keymap XKB mínimo válido para satisfazer o protocolo.
/// Necessário antes do primeiro key event em alguns compositors.
fn send_initial_keymap(state: &InjectorState) -> anyhow::Result<()> {
    let keyboard = state.keyboard.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Teclado virtual não disponível para keymap inicial.")
    })?;

    let initial = r#"xkb_keymap {
    xkb_keycodes "empty" { minimum = 8; maximum = 255; };
    xkb_types "empty" { include "complete" };
    xkb_compatibility "empty" { include "complete" };
    xkb_symbols "empty" { };
};"#;

    send_keymap_str(keyboard, initial)
}

// =============================================================================
// Estado do protocolo Wayland
// =============================================================================

struct InjectorState {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpVirtualKeyboardManagerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    qh: QueueHandle<InjectorState>,
}

// =============================================================================
// Handlers de eventos (maioria vazia — virtual keyboard é unidirecional)
// =============================================================================

impl Dispatch<wl_registry::WlRegistry, ()> for InjectorState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InjectorState {
    fn event(
        _: &mut Self, _: &wl_seat::WlSeat,
        _: wl_seat::Event, _: &(),
        _: &Connection, _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for InjectorState {
    fn event(
        _: &mut Self, _: &ZwpVirtualKeyboardManagerV1,
        _: zwp_virtual_keyboard_manager_v1::Event, _: &(),
        _: &Connection, _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for InjectorState {
    fn event(
        _: &mut Self, _: &ZwpVirtualKeyboardV1,
        _: zwp_virtual_keyboard_v1::Event, _: &(),
        _: &Connection, _: &QueueHandle<Self>,
    ) {}
}
