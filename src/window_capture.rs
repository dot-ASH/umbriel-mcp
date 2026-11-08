use anyhow::{Context, Result, anyhow};
use image::{ImageFormat, RgbImage};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{MemfdFlags, memfd_create};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1, ext_image_capture_source_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1, ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1,
};

const CAPTURE_DEADLINE: Duration = Duration::from_secs(5);
const BYTES_PER_PIXEL: u32 = 4;

pub enum NativeCapture {
    Captured(Vec<u8>),
    NoSuchWindow,
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder {
    BlueGreenRed,
    RedGreenBlue,
}

pub fn pixel_order(format: wl_shm::Format) -> Option<PixelOrder> {
    match format {
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(PixelOrder::BlueGreenRed),
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(PixelOrder::RedGreenBlue),
        _ => None,
    }
}

pub fn to_rgb(pixels: &[u8], width: u32, height: u32, stride: u32, order: PixelOrder) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        let start = (y * stride + x * BYTES_PER_PIXEL) as usize;
        let pixel = &pixels[start..start + 3];
        match order {
            PixelOrder::BlueGreenRed => image::Rgb([pixel[2], pixel[1], pixel[0]]),
            PixelOrder::RedGreenBlue => image::Rgb([pixel[0], pixel[1], pixel[2]]),
        }
    })
}

#[derive(Default)]
struct Toplevel {
    handle: Option<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1>,
    identifier: Option<String>,
}

#[derive(Default)]
struct CaptureState {
    toplevel_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    source_manager: Option<
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    >,
    copy_manager: Option<ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1>,
    shm: Option<wl_shm::WlShm>,
    toplevels: Vec<Toplevel>,
    buffer_size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    constraints_done: bool,
    session_stopped: bool,
    frame_outcome: Option<std::result::Result<(), String>>,
}

impl CaptureState {
    fn missing_protocol(&self) -> Option<&'static str> {
        if self.toplevel_list.is_none() {
            return Some("ext_foreign_toplevel_list_v1");
        }
        if self.source_manager.is_none() {
            return Some("ext_foreign_toplevel_image_capture_source_manager_v1");
        }
        if self.copy_manager.is_none() {
            return Some("ext_image_copy_capture_manager_v1");
        }
        if self.shm.is_none() {
            return Some("wl_shm");
        }
        None
    }

    fn chosen_format(&self) -> Option<(wl_shm::Format, PixelOrder)> {
        self.formats
            .iter()
            .find_map(|format| pixel_order(*format).map(|order| (*format, order)))
    }
}

fn dispatch_until(
    connection: &Connection,
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
    finished: impl Fn(&CaptureState) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + CAPTURE_DEADLINE;
    loop {
        queue.dispatch_pending(state)?;
        if finished(state) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!(
                "the compositor did not answer within {} seconds",
                CAPTURE_DEADLINE.as_secs()
            ));
        }
        connection.flush()?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let timeout = Timespec::try_from(remaining).unwrap_or(Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        });
        let connection_fd = guard.connection_fd();
        let mut descriptors = [PollFd::new(&connection_fd, PollFlags::IN)];
        if poll(&mut descriptors, Some(&timeout))? > 0 {
            guard.read()?;
        }
    }
}

fn read_frame(
    state: &mut CaptureState,
    queue: &mut EventQueue<CaptureState>,
    connection: &Connection,
    window_id: &str,
) -> Result<NativeCapture> {
    let handle = state
        .toplevels
        .iter()
        .find(|toplevel| toplevel.identifier.as_deref() == Some(window_id))
        .and_then(|toplevel| toplevel.handle.clone());
    let Some(handle) = handle else {
        return Ok(NativeCapture::NoSuchWindow);
    };
    let handle_queue = queue.handle();
    let source = state
        .source_manager
        .as_ref()
        .expect("checked above")
        .create_source(&handle, &handle_queue, ());
    let session = state
        .copy_manager
        .as_ref()
        .expect("checked above")
        .create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            &handle_queue,
            (),
        );
    dispatch_until(connection, queue, state, |state| {
        state.constraints_done || state.session_stopped
    })?;
    if state.session_stopped {
        return Err(anyhow!(
            "the compositor stopped the capture of window {window_id}"
        ));
    }
    let (width, height) = state
        .buffer_size
        .ok_or_else(|| anyhow!("the compositor gave no buffer size for window {window_id}"))?;
    let (format, order) = state.chosen_format().ok_or_else(|| {
        anyhow!("the compositor offered no 8-bit RGB shared-memory format for window {window_id}")
    })?;
    let stride = width * BYTES_PER_PIXEL;
    let length = stride as u64 * height as u64;
    let memory = memfd_create("umbriel-mcp-window-capture", MemfdFlags::CLOEXEC)
        .context("cannot create shared memory for the capture")?;
    let mut file = std::fs::File::from(memory);
    file.set_len(length)?;
    let shm = state.shm.as_ref().expect("checked above");
    let pool = shm.create_pool(file.as_fd(), length as i32, &handle_queue, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        format,
        &handle_queue,
        (),
    );
    let frame = session.create_frame(&handle_queue, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, width as i32, height as i32);
    frame.capture();
    let captured = dispatch_until(connection, queue, state, |state| {
        state.frame_outcome.is_some()
    });
    frame.destroy();
    session.destroy();
    source.destroy();
    buffer.destroy();
    pool.destroy();
    captured?;
    if let Some(Err(reason)) = state.frame_outcome.take() {
        return Err(anyhow!(
            "the compositor could not capture window {window_id}: {reason}"
        ));
    }
    let mut pixels = vec![0; length as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut pixels)?;
    let mut png = Vec::new();
    to_rgb(&pixels, width, height, stride, order)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(NativeCapture::Captured(png))
}

pub fn available() -> bool {
    let Ok(connection) = Connection::connect_to_env() else {
        return false;
    };
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut state = CaptureState::default();
    queue.roundtrip(&mut state).is_ok() && state.missing_protocol().is_none()
}

pub fn capture(window_id: &str) -> NativeCapture {
    let attempt = || -> Result<NativeCapture> {
        let connection = match Connection::connect_to_env() {
            Ok(connection) => connection,
            Err(error) => {
                return Ok(NativeCapture::Unavailable(format!(
                    "no Wayland connection ({error})"
                )));
            }
        };
        let mut queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut state = CaptureState::default();
        queue.roundtrip(&mut state)?;
        if let Some(missing) = state.missing_protocol() {
            return Ok(NativeCapture::Unavailable(format!(
                "the compositor does not offer {missing}"
            )));
        }
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        let outcome = read_frame(&mut state, &mut queue, &connection, window_id);
        for toplevel in &state.toplevels {
            if let Some(handle) = &toplevel.handle {
                handle.destroy();
            }
        }
        if let Some(list) = &state.toplevel_list {
            list.stop();
        }
        let _ = connection.flush();
        outcome
    };
    attempt().unwrap_or_else(|error| NativeCapture::Unavailable(format!("{error:#}")))
}

impl Dispatch<wl_registry::WlRegistry, ()> for CaptureState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name, interface, ..
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "ext_foreign_toplevel_list_v1" if state.toplevel_list.is_none() => {
                state.toplevel_list = Some(registry.bind(name, 1, queue, ()))
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1"
                if state.source_manager.is_none() =>
            {
                state.source_manager = Some(registry.bind(name, 1, queue, ()))
            }
            "ext_image_copy_capture_manager_v1" if state.copy_manager.is_none() => {
                state.copy_manager = Some(registry.bind(name, 1, queue, ()))
            }
            "wl_shm" if state.shm.is_none() => state.shm = Some(registry.bind(name, 1, queue, ())),
            _ => {}
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: Some(toplevel),
                identifier: None,
            });
        }
    }

    event_created_child!(CaptureState, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(toplevel) = state
            .toplevels
            .iter_mut()
            .find(|toplevel| toplevel.handle.as_ref().map(Proxy::id) == Some(handle.id()))
        else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                toplevel.identifier = Some(identifier)
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => toplevel.identifier = None,
            _ => {}
        }
    }
}

impl Dispatch<ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1, ()>
    for CaptureState
{
    fn event(
        state: &mut Self,
        _: &ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.buffer_size = Some((width, height))
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } => state.formats.push(format),
            ext_image_copy_capture_session_v1::Event::Done => state.constraints_done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => state.session_stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.frame_outcome = Some(Ok(())),
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                let reason = match reason {
                    WEnum::Value(
                        ext_image_copy_capture_frame_v1::FailureReason::BufferConstraints,
                    ) => "the buffer did not match its constraints".to_owned(),
                    WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::Stopped) => {
                        "the window went away".to_owned()
                    }
                    other => format!("{other:?}"),
                };
                state.frame_outcome = Some(Err(reason));
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),* $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for CaptureState {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

ignore_events!(
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
);

#[cfg(test)]
mod tests {
    use super::{PixelOrder, pixel_order, to_rgb};
    use wayland_client::protocol::wl_shm;

    #[test]
    fn little_endian_xrgb_pixels_are_stored_blue_first() {
        let pixels = [10, 20, 30, 255, 1, 2, 3, 0, 9, 9, 9, 9];
        let image = to_rgb(&pixels, 2, 1, 12, PixelOrder::BlueGreenRed);
        assert_eq!(image.get_pixel(0, 0).0, [30, 20, 10]);
        assert_eq!(image.get_pixel(1, 0).0, [3, 2, 1]);
    }

    #[test]
    fn rows_are_read_at_their_stride() {
        let pixels = [1, 2, 3, 0, 0, 0, 0, 0, 4, 5, 6, 0, 0, 0, 0, 0];
        let image = to_rgb(&pixels, 1, 2, 8, PixelOrder::RedGreenBlue);
        assert_eq!(image.get_pixel(0, 1).0, [4, 5, 6]);
    }

    #[test]
    fn only_8_bit_rgb_formats_are_used() {
        assert_eq!(
            pixel_order(wl_shm::Format::Xrgb8888),
            Some(PixelOrder::BlueGreenRed)
        );
        assert_eq!(
            pixel_order(wl_shm::Format::Abgr8888),
            Some(PixelOrder::RedGreenBlue)
        );
        assert_eq!(pixel_order(wl_shm::Format::Xrgb2101010), None);
    }
}
