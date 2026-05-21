/// clipboard.rs — Seleção primária do Wayland via `zwp_primary_selection_device_manager_v1`.
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

static SELECTION_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

pub fn set_primary_selection(text: &str) -> anyhow::Result<()> {
    let text = text.to_string();

    // FASE 3: Sintaxe idiomática estável para limpar a thread anterior
    if let Ok(mut guard) = SELECTION_THREAD.lock() {
        if let Some(prev) = guard.take() {
            drop(prev);
        }
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

    event_queue.roundtrip(&mut state)?;

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

    let source = manager.create_source(&qh, ());
    source.offer("text/plain;charset=utf-8".into());
    source.offer("text/plain".into());

    let device = manager.get_device(seat, &qh, ());
    device.set_selection(Some(&source), 0);

    state.source = Some(source);
    state.device = Some(device);

    event_queue.flush()?;
    debug!("Seleção primária definida. Aguardando pedidos de paste...");

    while !state.done {
        event_queue.blocking_dispatch(&mut state)?;
    }

    debug!("Seleção primária liberada (cancelled recebido).");
    Ok(())
}

struct SelectionState {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    device: Option<ZwpPrimarySelectionDeviceV1>,
    source: Option<ZwpPrimarySelectionSourceV1>,
    text: Arc<String>,
    done: bool,
}

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
            zwp_primary_selection_source_v1::Event::Send { mime_type: _, fd } => {
                serve_text(&state.text, fd);
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                debug!("Seleção primária cancelada pelo compositor.");
                state.done = true;
            }
            _ => {}
        }
    }
}

fn serve_text(text: &str, fd: OwnedFd) {
    let mut file: std::fs::File = fd.into();
    if let Err(e) = file.write_all(text.as_bytes()) {
        warn!("Falha ao escrever texto na seleção primária: {}", e);
    }
    let _ = file.flush();
}
