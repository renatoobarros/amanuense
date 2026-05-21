use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};

pub(super) struct InjectorState {
    pub(super) seat: Option<wl_seat::WlSeat>,
    pub(super) manager: Option<ZwpVirtualKeyboardManagerV1>,
    pub(super) keyboard: Option<ZwpVirtualKeyboardV1>,
    pub(super) qh: QueueHandle<InjectorState>,
}

impl InjectorState {
    pub(super) fn clone_for_dispatch(&self) -> Self {
        Self {
            seat: self.seat.clone(),
            manager: self.manager.clone(),
            keyboard: self.keyboard.clone(),
            qh: self.qh.clone(),
        }
    }
}

// =============================================================================
// Handlers de eventos
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
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for InjectorState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for InjectorState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
