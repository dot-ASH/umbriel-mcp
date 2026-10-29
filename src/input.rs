use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1;
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};
use wl_clipboard_rs::copy;
use wl_clipboard_rs::paste::{self, ClipboardType, MimeType as PasteMimeType, Seat};

#[derive(Clone, Copy)]
struct DesktopLayout {
    origin_x: i64,
    origin_y: i64,
    width: u32,
    height: u32,
}

impl DesktopLayout {
    fn from_outputs(outputs: &[Value]) -> Result<Self> {
        let mut enabled_outputs = outputs.iter().filter(|output| {
            output
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        let first = enabled_outputs
            .next()
            .ok_or_else(|| anyhow!("Umbriel reports no enabled outputs"))?;
        let mut min_x = output_position(first, "x")?;
        let mut min_y = output_position(first, "y")?;
        let mut max_x = min_x + output_dimension(first, "width")?;
        let mut max_y = min_y + output_dimension(first, "height")?;
        for output in enabled_outputs {
            let x = output_position(output, "x")?;
            let y = output_position(output, "y")?;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + output_dimension(output, "width")?);
            max_y = max_y.max(y + output_dimension(output, "height")?);
        }
        let width = u32::try_from(max_x - min_x)
            .context("Umbriel desktop layout width is outside the supported range")?;
        let height = u32::try_from(max_y - min_y)
            .context("Umbriel desktop layout height is outside the supported range")?;
        if width == 0 || height == 0 {
            return Err(anyhow!("Umbriel reports an empty desktop layout"));
        }
        Ok(Self {
            origin_x: min_x,
            origin_y: min_y,
            width,
            height,
        })
    }

    fn absolute_position(self, x: i32, y: i32) -> Result<(u32, u32)> {
        let relative_x = i64::from(x) - self.origin_x;
        let relative_y = i64::from(y) - self.origin_y;
        if relative_x < 0
            || relative_y < 0
            || relative_x >= i64::from(self.width)
            || relative_y >= i64::from(self.height)
        {
            return Err(anyhow!(
                "pointer position ({x},{y}) is outside Umbriel's desktop layout"
            ));
        }
        Ok((relative_x as u32, relative_y as u32))
    }
}

fn output_position(output: &Value, axis: &str) -> Result<i64> {
    output
        .get("position")
        .and_then(|position| position.get(axis))
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Umbriel output is missing position.{axis}"))
}

fn output_dimension(output: &Value, dimension: &str) -> Result<i64> {
    output
        .get("logical_size")
        .and_then(|size| size.get(dimension))
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("Umbriel output is missing a valid logical_size.{dimension}"))
}

#[derive(Default)]
pub(crate) struct InputGlobals {
    pub(crate) seat: Option<wl_seat::WlSeat>,
    pointer_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    pub(crate) keyboard_manager:
        Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
}

pub fn input_protocol_support() -> Result<(bool, bool)> {
    let (_connection, _queue, globals) = discover_input_globals()?;
    Ok((
        globals.pointer_manager.is_some(),
        globals.keyboard_manager.is_some(),
    ))
}

pub(crate) fn discover_input_globals()
-> Result<(Connection, EventQueue<InputGlobals>, InputGlobals)> {
    let connection = Connection::connect_to_env()
        .context("cannot connect to Wayland; check WAYLAND_DISPLAY and XDG_RUNTIME_DIR")?;
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut globals = InputGlobals::default();
    queue
        .roundtrip(&mut globals)
        .context("could not discover Wayland input protocols")?;
    Ok((connection, queue, globals))
}

impl Dispatch<wl_registry::WlRegistry, ()> for InputGlobals {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, version.min(7), queue, ()))
            }
            "zwlr_virtual_pointer_manager_v1" if state.pointer_manager.is_none() => {
                state.pointer_manager = Some(registry.bind(name, version.min(2), queue, ()))
            }
            "zwp_virtual_keyboard_manager_v1" if state.keyboard_manager.is_none() => {
                state.keyboard_manager = Some(registry.bind(name, version.min(1), queue, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InputGlobals {
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

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for InputGlobals {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        _: zwlr_virtual_pointer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for InputGlobals {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _: zwlr_virtual_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for InputGlobals {
    fn event(
        _: &mut Self,
        _: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _: zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct WaylandPointer {
    connection: Connection,
    _queue: EventQueue<InputGlobals>,
    _globals: InputGlobals,
    pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    layout: DesktopLayout,
    started_at: Instant,
}

impl WaylandPointer {
    fn connect() -> Result<Self> {
        let (connection, mut queue, mut globals) = discover_input_globals()?;
        let queue_handle = queue.handle();
        let seat = globals
            .seat
            .as_ref()
            .ok_or_else(|| anyhow!("Umbriel does not advertise the Wayland seat protocol"))?;
        let manager = globals.pointer_manager.as_ref().ok_or_else(|| {
            anyhow!(
                "Umbriel does not expose zwlr_virtual_pointer_manager_v1; check for a restricted Wayland session"
            )
        })?;
        let output_response = crate::ipc::query("outputs")?;
        let outputs = output_response
            .as_array()
            .ok_or_else(|| anyhow!("Umbriel returned an invalid output list"))?;
        let layout = DesktopLayout::from_outputs(outputs)?;
        let pointer = manager.create_virtual_pointer(Some(seat), &queue_handle, ());
        queue
            .roundtrip(&mut globals)
            .context("could not initialize Umbriel's virtual pointer")?;
        Ok(Self {
            connection,
            _queue: queue,
            _globals: globals,
            pointer,
            layout,
            started_at: Instant::now(),
        })
    }

    fn time(&self) -> u32 {
        self.started_at.elapsed().as_millis().min(u32::MAX as u128) as u32
    }

    fn flush(&self, action: &str) -> Result<()> {
        self.connection
            .flush()
            .with_context(|| format!("could not send virtual pointer {action} to Wayland"))
    }

    fn move_to(&self, x: i32, y: i32) -> Result<()> {
        let (x, y) = self.layout.absolute_position(x, y)?;
        self.pointer
            .motion_absolute(self.time(), x, y, self.layout.width, self.layout.height);
        self.pointer.frame();
        self.flush("movement")
    }

    fn button(&self, button: &str, state: wl_pointer::ButtonState) -> Result<()> {
        let code = pointer_button_code(button)?;
        self.pointer.button(self.time(), code, state);
        self.pointer.frame();
        self.flush("button")
    }

    fn scroll(&self, direction: &str, amount: i32) -> Result<()> {
        let distance = match direction {
            "down" => amount,
            "up" => -amount,
            other => return Err(anyhow!("unknown direction {other}; expected up or down")),
        };
        self.pointer.axis(
            self.time(),
            wl_pointer::Axis::VerticalScroll,
            f64::from(distance),
        );
        self.pointer.frame();
        self.flush("scroll")
    }
}

static WAYLAND_POINTER: OnceLock<Mutex<WaylandPointer>> = OnceLock::new();

fn wayland_pointer() -> Result<&'static Mutex<WaylandPointer>> {
    if let Some(pointer) = WAYLAND_POINTER.get() {
        return Ok(pointer);
    }
    let pointer = WaylandPointer::connect()?;
    let _ = WAYLAND_POINTER.set(Mutex::new(pointer));
    WAYLAND_POINTER
        .get()
        .ok_or_else(|| anyhow!("could not retain the Wayland virtual pointer"))
}

fn pointer_button_code(button: &str) -> Result<u32> {
    match button {
        "left" => Ok(0x110),
        "right" => Ok(0x111),
        "middle" => Ok(0x112),
        "side" => Ok(0x116),
        "forward" => Ok(0x115),
        other => Err(anyhow!(
            "unknown button {other}; expected left, right, middle, side or forward"
        )),
    }
}

fn locked_pointer() -> Result<std::sync::MutexGuard<'static, WaylandPointer>> {
    wayland_pointer()?
        .lock()
        .map_err(|_| anyhow!("Wayland pointer lock was poisoned"))
}

pub fn move_pointer(x: i32, y: i32) -> Result<()> {
    locked_pointer()?.move_to(x, y)
}

pub fn click(button: &str, double: bool) -> Result<()> {
    pointer_button_code(button)?;
    let pointer = locked_pointer()?;
    let clicks = if double { 2 } else { 1 };
    for index in 0..clicks {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        pointer.button(button, wl_pointer::ButtonState::Pressed)?;
        pointer.button(button, wl_pointer::ButtonState::Released)?;
    }
    Ok(())
}

pub fn scroll(direction: &str, amount: i32) -> Result<()> {
    locked_pointer()?.scroll(direction, amount)
}

pub fn drag(from: (i32, i32), to: (i32, i32), button: &str) -> Result<()> {
    pointer_button_code(button)?;
    let pointer = locked_pointer()?;
    pointer.layout.absolute_position(from.0, from.1)?;
    pointer.layout.absolute_position(to.0, to.1)?;
    pointer.move_to(from.0, from.1)?;
    pointer.button(button, wl_pointer::ButtonState::Pressed)?;
    let moved = pointer.move_to(to.0, to.1);
    let released = pointer.button(button, wl_pointer::ButtonState::Released);
    moved?;
    released
}

pub use crate::keyboard::{press_keys, type_text};

pub fn clipboard_read() -> Result<String> {
    let (mut reader, _) = paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Text,
    )
    .map_err(describe_paste_failure)?;
    let mut contents = Vec::new();
    reader
        .read_to_end(&mut contents)
        .context("could not read the clipboard contents")?;
    String::from_utf8(contents).map_err(|_| anyhow!("the clipboard text is not valid UTF-8"))
}

fn describe_paste_failure(error: paste::Error) -> anyhow::Error {
    match error {
        paste::Error::ClipboardEmpty => anyhow!("the clipboard is empty"),
        paste::Error::NoMimeType => anyhow!("the clipboard holds no text"),
        other => anyhow!("could not read the clipboard: {other}"),
    }
}

pub fn clipboard_write(text: &str) -> Result<()> {
    copy::Options::new()
        .copy(
            copy::Source::Bytes(text.as_bytes().into()),
            copy::MimeType::Text,
        )
        .map_err(|error| anyhow!("could not write the clipboard: {error}"))
}

#[cfg(test)]
mod tests {
    use super::DesktopLayout;
    use serde_json::json;

    #[test]
    fn absolute_pointer_extent_spans_all_enabled_outputs() {
        let outputs = json!([
            {"enabled":true,"position":{"x":0,"y":0},"logical_size":{"width":1920,"height":1080}},
            {"enabled":true,"position":{"x":1920,"y":0},"logical_size":{"width":1920,"height":1080}},
            {"enabled":false,"position":{"x":3840,"y":0},"logical_size":{"width":1920,"height":1080}}
        ]);
        let layout = DesktopLayout::from_outputs(outputs.as_array().unwrap()).unwrap();
        assert_eq!((layout.width, layout.height), (3840, 1080));
        assert_eq!(layout.absolute_position(2000, 100).unwrap(), (2000, 100));
    }

    #[test]
    fn absolute_pointer_coordinates_account_for_negative_output_origins() {
        let outputs = json!([
            {"enabled":true,"position":{"x":-1280,"y":-200},"logical_size":{"width":1280,"height":1024}},
            {"enabled":true,"position":{"x":0,"y":0},"logical_size":{"width":1920,"height":1080}}
        ]);
        let layout = DesktopLayout::from_outputs(outputs.as_array().unwrap()).unwrap();
        assert_eq!((layout.origin_x, layout.origin_y), (-1280, -200));
        assert_eq!((layout.width, layout.height), (3200, 1280));
        assert_eq!(layout.absolute_position(-1280, -200).unwrap(), (0, 0));
        assert_eq!(layout.absolute_position(0, 0).unwrap(), (1280, 200));
    }

    #[test]
    fn absolute_pointer_rejects_positions_outside_desktop_layout() {
        let layout = DesktopLayout {
            origin_x: 0,
            origin_y: 0,
            width: 3840,
            height: 1080,
        };
        assert!(layout.absolute_position(-1, 0).is_err());
        assert!(layout.absolute_position(3840, 0).is_err());
        assert!(layout.absolute_position(0, 1080).is_err());
    }
}
