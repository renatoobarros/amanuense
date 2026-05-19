use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::Duration;

use tracing::debug;
use wayland_client::{Connection, EventQueue, protocol::wl_keyboard};

mod keymap;
mod memfd;
mod protocol;

use memfd::send_keymap_str;
use protocol::{InjectorState, send_initial_keymap};

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

        let manager = state.manager.as_ref().ok_or_else(|| {
            anyhow::anyhow!("zwp_virtual_keyboard_manager_v1 não encontrado no compositor.")
        })?;

        let seat = state
            .seat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wl_seat não encontrado no compositor."))?;

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

    pub fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        let keyboard = self
            .state
            .keyboard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Teclado virtual não inicializado."))?
            .clone();

        debug!(
            "Injetando {} caracteres via teclado virtual.",
            text.chars().count()
        );

        // Quebra o texto em partes para não ultrapassar a disponibilidade de teclas no XKB
        let mut current_chunk = String::new();
        let mut current_uniques = HashSet::new();
        let mut chunks = Vec::new();

        for ch in text.chars() {
            if !current_uniques.contains(&ch) && current_uniques.len() >= 200 {
                chunks.push(current_chunk);
                current_chunk = String::new();
                current_uniques.clear();
            }
            current_uniques.insert(ch);
            current_chunk.push(ch);
        }
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        let mut t = self.time_counter;

        // Processa cada bloco de texto e envia FD único por bloco
        for chunk in chunks {
            let uniques: HashSet<char> = chunk.chars().collect();
            let mut char_map = HashMap::new();
            let mut map_vec = Vec::new();
            let mut base_code = 30; // Inicializando o offset de evdev a partir do KEY_A

            for &ch in &uniques {
                char_map.insert(ch, base_code);
                map_vec.push((ch, base_code));
                base_code += 1;
            }

            let keymap_str = keymap::build_bulk_keymap(&map_vec);
            send_keymap_str(&keyboard, &keymap_str)?;

            // Pausa sutil para absorção segura do keymap pelo Wayland
            thread::sleep(Duration::from_millis(5));

            for ch in chunk.chars() {
                let code = char_map[&ch];
                keyboard.key(t, code, wl_keyboard::KeyState::Pressed.into());
                keyboard.key(t + 10, code, wl_keyboard::KeyState::Released.into());
                t += 20;
            }

            // Um único flush por bloco é suficiente agora que unificamos o map
            self.conn.flush()?;
            thread::sleep(Duration::from_millis(10));
        }

        self.time_counter = t;

        let mut state = InjectorState {
            seat: self.state.seat.clone(),
            manager: self.state.manager.clone(),
            keyboard: self.state.keyboard.clone(),
            qh: self.state.qh.clone(),
        };
        let _ = self.event_queue.dispatch_pending(&mut state);

        debug!("Injeção concluída.");
        Ok(())
    }
}
