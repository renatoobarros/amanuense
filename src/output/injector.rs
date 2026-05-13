/// injector/ — Injeção de texto via `zwp_virtual_keyboard_v1`.
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
/// LGPD: keymaps são transmitidos via `memfd_create(2)` (arquivo em memória,
/// RAM-only). O texto nunca toca o disco nem é enviado pela rede.
use tracing::debug;
use wayland_client::{Connection, EventQueue, protocol::wl_keyboard};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

mod keymap;
mod memfd;
mod protocol;

use keymap::{build_keysym_keymap, build_unicode_keymap};
use memfd::send_keymap_str;
use protocol::{InjectorState, send_initial_keymap};

// Código de tecla físico usado como "slot" para injeção de caracteres.
// KEY_A (30) é arbitrário — qualquer tecla serve, usamos sempre a mesma.
pub(super) const INJECT_KEY_CODE: u32 = 30;

// Tempo simulado entre press e release (em ms). O Wayland usa timestamps
// relativos — usamos valores incrementais simples.
const KEY_RELEASE_TIME: u32 = 200;

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

        let seat = state
            .seat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wl_seat não encontrado no compositor."))?;

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
    pub fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        let keyboard = self
            .state
            .keyboard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Teclado virtual não inicializado."))?;

        debug!(
            "Injetando {} caracteres via teclado virtual.",
            text.chars().count()
        );

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
        self.conn
            .flush()
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
    keyboard.key(
        press_time,
        INJECT_KEY_CODE,
        wl_keyboard::KeyState::Pressed.into(),
    );
    // Release
    keyboard.key(
        release_time,
        INJECT_KEY_CODE,
        wl_keyboard::KeyState::Released.into(),
    );

    Ok(())
}

/// Injeta um caractere Unicode arbitrário criando um keymap XKB temporário.
fn inject_unicode_char(
    keyboard: &ZwpVirtualKeyboardV1,
    ch: char,
    press_time: u32,
    release_time: u32,
) -> anyhow::Result<()> {
    let keymap_str = build_unicode_keymap(ch);
    send_keymap_str(keyboard, &keymap_str)?;

    keyboard.key(
        press_time,
        INJECT_KEY_CODE,
        wl_keyboard::KeyState::Pressed.into(),
    );
    keyboard.key(
        release_time,
        INJECT_KEY_CODE,
        wl_keyboard::KeyState::Released.into(),
    );

    Ok(())
}
