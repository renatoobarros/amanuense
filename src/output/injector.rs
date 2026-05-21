use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::io::OwnedFd;
use std::thread;
use std::time::Duration;
use tracing::debug;
use wayland_client::backend::WaylandError;
use wayland_client::{Connection, EventQueue, protocol::wl_keyboard};

mod keymap;
mod memfd;
mod protocol;

use keymap::build_xkb_string;
use memfd::create_and_send_keymap;
use protocol::InjectorState;

const KEY_RELEASE_TIME: u32 = 200;

pub struct TextInjector {
    conn: Connection,
    event_queue: EventQueue<InjectorState>,
    state: InjectorState,
    time_counter: std::num::Wrapping<u32>,
    char_to_keycode: HashMap<char, u32>,
    next_keycode: u32,
    _active_keymap_fd: Option<OwnedFd>,
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

        let mut injector = Self {
            conn,
            event_queue,
            state,
            time_counter: std::num::Wrapping(KEY_RELEASE_TIME),
            char_to_keycode: HashMap::new(),
            next_keycode: 8,
            _active_keymap_fd: None,
        };

        injector.populate_standard_keymap();
        injector.update_keymap()?;

        debug!("Teclado virtual Wayland inicializado em modo lote.");
        Ok(injector)
    }

    fn populate_standard_keymap(&mut self) {
        for c in 32..=126 {
            self.char_to_keycode
                .insert(c as u8 as char, self.next_keycode);
            self.next_keycode += 1;
        }
        self.char_to_keycode.insert('\n', self.next_keycode);
        self.next_keycode += 1;
        self.char_to_keycode.insert('\t', self.next_keycode);
        self.next_keycode += 1;

        let pt_chars = [
            'á', 'à', 'â', 'ã', 'é', 'ê', 'í', 'ó', 'ô', 'õ', 'ú', 'ç', 'Á', 'À', 'Â', 'Ã', 'É',
            'Ê', 'Í', 'Ó', 'Ô', 'Õ', 'Ú', 'Ç',
        ];
        for c in pt_chars {
            if !self.char_to_keycode.contains_key(&c) {
                self.char_to_keycode.insert(c, self.next_keycode);
                self.next_keycode += 1;
            }
        }
    }

    fn update_keymap(&mut self) -> anyhow::Result<()> {
        let kb = self
            .state
            .keyboard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Teclado virtual não inicializado."))?;

        let xkb_str = build_xkb_string(&self.char_to_keycode);
        let fd = create_and_send_keymap(kb, &xkb_str)?;
        self._active_keymap_fd = Some(fd);

        let _ = self.conn.flush();
        let mut dummy = self.state.clone_for_dispatch();
        let _ = self.event_queue.roundtrip(&mut dummy);

        Ok(())
    }

    pub fn type_text(&mut self, text: &str, delay_ms: u32) -> anyhow::Result<()> {
        let mut needs_update = false;

        for ch in text.chars() {
            if !self.char_to_keycode.contains_key(&ch) {
                if self.next_keycode <= 255 {
                    self.char_to_keycode.insert(ch, self.next_keycode);
                    self.next_keycode += 1;
                    needs_update = true;
                } else {
                    self.char_to_keycode.clear();
                    self.next_keycode = 8;
                    self.populate_standard_keymap();
                    self.char_to_keycode.insert(ch, self.next_keycode);
                    self.next_keycode += 1;
                    needs_update = true;
                }
            }
        }

        if needs_update {
            self.update_keymap()?;
        }

        let keyboard = self
            .state
            .keyboard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Teclado virtual não inicializado pelo compositor."))?
            .clone();

        for (i, ch) in text.chars().enumerate() {
            if let Some(&keycode) = self.char_to_keycode.get(&ch) {
                let pt = self.time_counter.0;
                let rt = (self.time_counter + std::num::Wrapping(10)).0;

                // A correção do offset: Wayland espera evdev, não XKB bruto.
                let evdev_keycode = keycode - 8;

                keyboard.key(pt, evdev_keycode, wl_keyboard::KeyState::Pressed.into());
                keyboard.key(rt, evdev_keycode, wl_keyboard::KeyState::Released.into());

                self.time_counter += std::num::Wrapping(delay_ms);

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

                thread::sleep(Duration::from_millis(delay_ms as u64));

                if i > 0 && i % 64 == 0 {
                    let mut dummy = self.state.clone_for_dispatch();
                    let _ = self.event_queue.roundtrip(&mut dummy);
                }
            }
        }

        let mut dummy = self.state.clone_for_dispatch();
        let _ = self.event_queue.dispatch_pending(&mut dummy);

        Ok(())
    }
}
