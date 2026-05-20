use std::io::ErrorKind;
use std::thread;
use std::time::Duration;
use tracing::debug;
use wayland_client::backend::WaylandError;
use wayland_client::{Connection, EventQueue, protocol::wl_keyboard};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

mod keymap;
mod memfd;
mod protocol;

use keymap::{build_keysym_keymap, build_unicode_keymap};
use memfd::send_keymap_str;
use protocol::{InjectorState, send_initial_keymap};

pub(super) const INJECT_KEY_CODE: u32 = 30;
const KEY_RELEASE_TIME: u32 = 200;

pub struct TextInjector {
    conn: Connection,
    event_queue: EventQueue<InjectorState>,
    state: InjectorState,
    time_counter: u32,
}

impl TextInjector {
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

        event_queue.roundtrip(&mut state)?;

        let manager = state
            .manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("zwp_virtual_keyboard_manager_v1 não encontrado."))?;

        let seat = state
            .seat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wl_seat não encontrado."))?;

        let keyboard = manager.create_virtual_keyboard(seat, &qh, ());
        state.keyboard = Some(keyboard);

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

    pub fn type_text(&mut self, text: &str, delay_ms: u32) -> anyhow::Result<()> {
        let keyboard = self.state.keyboard.as_ref().unwrap().clone();
        let mut t = self.time_counter;

        for (i, ch) in text.chars().enumerate() {
            match ch {
                '\n' => inject_keysym(&keyboard, 0xff0d, t, t + 10)?,
                '\t' => inject_keysym(&keyboard, 0xff09, t, t + 10)?,
                ' ' => inject_keysym(&keyboard, 0x0020, t, t + 10)?,
                c => inject_unicode_char(&keyboard, c, t, t + 10)?,
            }

            // O tempo virtual do Wayland avança no mesmo ritmo do seu delay
            t += delay_ms;

            loop {
                match self.conn.flush() {
                    Ok(_) => break,
                    Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(WaylandError::Io(e)) if e.raw_os_error() == Some(109) => {
                        thread::sleep(Duration::from_millis(15));
                    }
                    Err(e) => return Err(anyhow::anyhow!("Falha ao enviar evento: {}", e)),
                }
            }

            // A pausa real controlada agora pelo config.toml
            thread::sleep(Duration::from_millis(delay_ms as u64));

            if i > 0 && i % 64 == 0 {
                let mut state = InjectorState {
                    seat: self.state.seat.clone(),
                    manager: self.state.manager.clone(),
                    keyboard: self.state.keyboard.clone(),
                    qh: self.state.qh.clone(),
                };
                let _ = self.event_queue.roundtrip(&mut state);
            }
        }

        self.time_counter = t;

        let mut state = InjectorState {
            seat: self.state.seat.clone(),
            manager: self.state.manager.clone(),
            keyboard: self.state.keyboard.clone(),
            qh: self.state.qh.clone(),
        };
        let _ = self.event_queue.dispatch_pending(&mut state);

        Ok(())
    }
}

fn inject_keysym(
    keyboard: &ZwpVirtualKeyboardV1,
    keysym: u32,
    pt: u32,
    rt: u32,
) -> anyhow::Result<()> {
    let keymap_str = build_keysym_keymap(keysym);
    send_keymap_str(keyboard, &keymap_str)?;
    keyboard.key(pt, INJECT_KEY_CODE, wl_keyboard::KeyState::Pressed.into());
    keyboard.key(rt, INJECT_KEY_CODE, wl_keyboard::KeyState::Released.into());
    Ok(())
}

fn inject_unicode_char(
    keyboard: &ZwpVirtualKeyboardV1,
    ch: char,
    pt: u32,
    rt: u32,
) -> anyhow::Result<()> {
    let keymap_str = build_unicode_keymap(ch);
    send_keymap_str(keyboard, &keymap_str)?;
    keyboard.key(pt, INJECT_KEY_CODE, wl_keyboard::KeyState::Pressed.into());
    keyboard.key(rt, INJECT_KEY_CODE, wl_keyboard::KeyState::Released.into());
    Ok(())
}
