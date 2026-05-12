/// clipboard.rs — Seleção primária do Wayland via `zwp_primary_selection_device_manager_v1`.
///
/// A seleção primária é o mecanismo Unix de "copiar ao selecionar, colar com
/// botão do meio". Diferente da área de transferência convencional, ela é
/// efêmera e não requer Ctrl+C explícito.
///
/// Protocolo utilizado: `primary-selection-unstable-v1`
/// Suportado por: wlroots, niri, sway, KDE Plasma 5.18+
///
/// Fluxo do protocolo:
///   1. Criamos uma `ZwpPrimarySelectionSourceV1` e oferecemos `text/plain;charset=utf-8`
///   2. Chamamos `device.set_selection(source, serial=0)` — serial 0 é aceito
///      por compositors wlroots para operações programáticas
///   3. Rodamos o event loop em thread dedicada para atender requisições de paste
///   4. Quando o compositor enviar `send(mime, fd)` → escrevemos o texto no fd
///   5. Quando recebemos `cancelled` → outra fonte tomou a seleção, encerramos
///
/// LGPD: o texto trafega apenas em memória (RAM) e via socket Wayland (IPC local).
use std::io::Write;
use std::os::unix::io::OwnedFd;
use std::sync::{Arc, Mutex};
use std::thread;

use tracing::{debug, warn};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::{self, ZwpPrimarySelectionDeviceManagerV1},
    zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1},
};

// =============================================================================
// Controle de thread de seleção ativa
// =============================================================================

/// Thread de seleção ativa. Quando uma nova seleção é definida, a anterior
/// é cancelada automaticamente pelo compositor (evento `cancelled`).
/// Usamos este Mutex apenas para aguardar a thread anterior encerrar.
static SELECTION_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

// =============================================================================
// API pública
// =============================================================================

/// Define o texto como seleção primária do Wayland.
///
/// Conecta ao compositor em uma thread dedicada, define a seleção e mantém
/// a thread ativa para responder a pedidos de paste enquanto a seleção for
/// nossa. A thread encerra automaticamente quando o usuário selecionar texto
/// em outro aplicativo (evento `cancelled`).
pub fn set_primary_selection(text: &str) -> anyhow::Result<()> {
    let text = text.to_string();

    // A seleção anterior será cancelada pelo compositor automaticamente.
    // Aguardamos a thread anterior para evitar acúmulo de threads zumbis.
    if let Ok(mut guard) = SELECTION_THREAD.lock()
        && let Some(prev) = guard.take()
    {
        // Non-blocking: se a thread anterior ainda estiver ativa, deixamos
        // ela encerrar sozinha (o evento `cancelled` chegará em breve).
        drop(prev);
    }

    let handle = thread::Builder::new()
        .name("whisper-primary-sel".into())
        .spawn(move || {
            if let Err(e) = run_selection_owner(text) {
                warn!("Falha na thread de seleção primária: {}", e);
            }
            debug!("Thread de seleção primária encerrada.");
        })
        .map_err(|e| anyhow::anyhow!("Falha ao criar thread de seleção: {}", e))?;

    if let Ok(mut guard) = SELECTION_THREAD.lock() {
        *guard = Some(handle);
    }

    Ok(())
}

// =============================================================================
// Loop de dono da seleção (thread dedicada)
// =============================================================================

fn run_selection_owner(text: String) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow::anyhow!("Falha ao conectar ao Wayland: {}", e))?;

    let mut event_queue: EventQueue<SelectionState> = conn.new_event_queue();
    let qh = event_queue.handle();

    let display = conn.display();
    display.get_registry(&qh, ());

    let mut state = SelectionState {
        seat: None,
        manager: None,
        device: None,
        source: None,
        text: Arc::new(text),
        done: false,
    };

    // Primeiro roundtrip: obtém globals (seat e manager)
    event_queue.roundtrip(&mut state)?;

    // Valida que os globals necessários foram encontrados
    let manager = state.manager.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "zwp_primary_selection_device_manager_v1 não encontrado. \
             Verifique se o compositor suporta primary-selection-unstable-v1."
        )
    })?;

    let seat = state
        .seat
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("wl_seat não encontrado no compositor."))?;

    // Cria a fonte de dados e oferece o tipo MIME
    let source = manager.create_source(&qh, ());
    source.offer("text/plain;charset=utf-8".into());
    source.offer("text/plain".into());

    // Obtém o dispositivo de seleção primária para este seat
    let device = manager.get_device(seat, &qh, ());

    // Define a seleção (serial 0 aceito por wlroots para operações programáticas)
    device.set_selection(Some(&source), 0);

    state.source = Some(source);
    state.device = Some(device);

    // Flush inicial: envia os requests acima ao compositor
    event_queue.flush()?;

    debug!("Seleção primária definida. Aguardando pedidos de paste...");

    // Loop de eventos: responde a `send` e encerra em `cancelled`
    while !state.done {
        event_queue.blocking_dispatch(&mut state)?;
    }

    debug!("Seleção primária liberada (cancelled recebido).");
    Ok(())
}

// =============================================================================
// Estado do protocolo
// =============================================================================

struct SelectionState {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    device: Option<ZwpPrimarySelectionDeviceV1>,
    source: Option<ZwpPrimarySelectionSourceV1>,
    text: Arc<String>,
    done: bool,
}

// =============================================================================
// Handlers de eventos Wayland
// =============================================================================

impl Dispatch<wl_registry::WlRegistry, ()> for SelectionState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwp_primary_selection_device_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

// Eventos do wl_seat que não precisamos processar
impl Dispatch<wl_seat::WlSeat, ()> for SelectionState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// Eventos do manager: nenhum
impl Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for SelectionState {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceManagerV1,
        _: zwp_primary_selection_device_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// Eventos do device: nenhum relevante para nós como fonte
impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for SelectionState {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        _: zwp_primary_selection_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Eventos da fonte: aqui é onde servimos o texto e detectamos o cancelamento.
impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for SelectionState {
    fn event(
        state: &mut Self,
        _source: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // O compositor pede que escrevamos os dados no fd fornecido
            zwp_primary_selection_source_v1::Event::Send { mime_type: _, fd } => {
                serve_text(&state.text, fd);
            }

            // Outra fonte tomou a seleção — podemos encerrar
            zwp_primary_selection_source_v1::Event::Cancelled => {
                debug!("Seleção primária cancelada pelo compositor.");
                state.done = true;
            }

            _ => {}
        }
    }
}

// =============================================================================
// Serviço de dados
// =============================================================================

/// Escreve o texto no file descriptor fornecido pelo compositor.
/// O fd tem vida útil própria (OwnedFd) — será fechado ao sair da função.
fn serve_text(text: &str, fd: OwnedFd) {
    // OwnedFd → File: seguro porque somos os únicos donos do fd
    let mut file: std::fs::File = fd.into();
    if let Err(e) = file.write_all(text.as_bytes()) {
        warn!("Falha ao escrever texto na seleção primária: {}", e);
    }
    // `file` é dropped aqui → fd é fechado → compositor sabe que os dados chegaram
}
