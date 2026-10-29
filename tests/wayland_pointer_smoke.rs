use rustix::event::{PollFd, PollFlags, Timespec, poll};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

#[derive(Clone)]
struct OutputTarget {
    proxy: wl_output::WlOutput,
    name: Option<String>,
}

#[derive(Default)]
struct ProbeState {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    outputs: Vec<OutputTarget>,
    layer_shell: Option<ZwlrLayerShellV1>,
    configured_surfaces: Option<Arc<AtomicU32>>,
    output_positions: HashMap<String, (i32, i32)>,
    surface_names: HashMap<ObjectId, String>,
    focused_output: Option<String>,
    last_global_position: Arc<Mutex<Option<(i32, i32)>>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
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
            "wl_compositor" if state.compositor.is_none() => {
                state.compositor = Some(registry.bind(name, version.min(5), queue, ()))
            }
            "wl_shm" if state.shm.is_none() => state.shm = Some(registry.bind(name, 1, queue, ())),
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, version.min(7), queue, ()))
            }
            "wl_output" => {
                let proxy = registry.bind(name, version.min(4), queue, ());
                state.outputs.push(OutputTarget { proxy, name: None });
            }
            "zwlr_layer_shell_v1" if state.layer_shell.is_none() => {
                state.layer_shell = Some(registry.bind(name, version.min(4), queue, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for ProbeState {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(entry) = state
                .outputs
                .iter_mut()
                .find(|entry| entry.proxy.id() == output.id())
            {
                entry.name = Some(name);
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ProbeState {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_seat::Event::Capabilities { .. }) {
            seat.get_pointer(queue, ());
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let local = match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.focused_output = state.surface_names.get(&surface.id()).cloned();
                Some((surface_x, surface_y))
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => Some((surface_x, surface_y)),
            _ => None,
        };
        let (Some((local_x, local_y)), Some(output)) = (local, &state.focused_output) else {
            return;
        };
        if let Some((origin_x, origin_y)) = state.output_positions.get(output) {
            *state.last_global_position.lock().unwrap() = Some((
                origin_x + local_x.round() as i32,
                origin_y + local_y.round() as i32,
            ));
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for ProbeState {
    fn event(
        state: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            surface.ack_configure(serial);
            if let Some(configured) = &state.configured_surfaces {
                configured.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

macro_rules! ignore_dispatch {
    ($($interface:ty),+ $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for ProbeState {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {}
            }
        )+
    };
}

ignore_dispatch!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface,
    wl_keyboard::WlKeyboard,
    ZwlrLayerShellV1
);

struct McpProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start MCP server");
        let input = child.stdin.take().expect("MCP stdin");
        let output = BufReader::new(child.stdout.take().expect("MCP stdout"));
        let mut process = Self {
            child,
            input,
            output,
            next_id: 1,
        };
        let initialized = process.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "wayland-pointer-smoke", "version": "1" }
            }),
        );
        assert_eq!(initialized["result"]["serverInfo"]["name"], "umbriel-mcp");
        process.notify("notifications/initialized", json!({}));
        process
    }

    fn notify(&mut self, method: &str, params: Value) {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0", "method":method, "params":params})
        )
        .expect("write MCP notification");
        self.input.flush().expect("flush MCP notification");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
        )
        .expect("write MCP request");
        self.input.flush().expect("flush MCP request");
        loop {
            let mut line = String::new();
            self.output.read_line(&mut line).expect("read MCP response");
            assert!(!line.is_empty(), "MCP server stopped unexpectedly");
            let response: Value = serde_json::from_str(&line).expect("MCP JSON response");
            if response["id"].as_u64() == Some(id) {
                return response;
            }
        }
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name, "arguments":arguments}))
    }

    fn move_pointer(&mut self, x: i32, y: i32) {
        let response = self.call_tool("mouse_move", json!({"x":x,"y":y}));
        assert!(
            response.get("result").is_some(),
            "mouse_move failed: {response}"
        );
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct OutputPlacement {
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn output_placements() -> (i32, i32, u32, u32, Vec<OutputPlacement>) {
    let socket_path = std::env::var("UMBRIEL_SOCKET").unwrap_or_else(|_| {
        format!(
            "{}/umbriel-{}.sock",
            std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR"),
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into())
        )
    });
    let mut stream = UnixStream::connect(socket_path).expect("connect to Umbriel IPC");
    writeln!(stream, "{}", json!({"cmd":"outputs"})).expect("request output list");
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("read output list");
    let reply: Value = serde_json::from_str(&line).expect("output JSON");
    let outputs = reply["ok"].as_array().expect("Umbriel outputs");
    let enabled: Vec<OutputPlacement> = outputs
        .iter()
        .filter(|output| output["enabled"].as_bool() == Some(true))
        .map(|output| OutputPlacement {
            name: output["name"].as_str().expect("output name").to_string(),
            x: output["position"]["x"].as_i64().expect("output x") as i32,
            y: output["position"]["y"].as_i64().expect("output y") as i32,
            width: output["logical_size"]["width"]
                .as_u64()
                .expect("output width") as u32,
            height: output["logical_size"]["height"]
                .as_u64()
                .expect("output height") as u32,
        })
        .collect();
    let min_x = enabled
        .iter()
        .map(|output| output.x)
        .min()
        .expect("enabled outputs");
    let min_y = enabled
        .iter()
        .map(|output| output.y)
        .min()
        .expect("enabled outputs");
    let max_x = enabled
        .iter()
        .map(|output| output.x + output.width as i32)
        .max()
        .unwrap();
    let max_y = enabled
        .iter()
        .map(|output| output.y + output.height as i32)
        .max()
        .unwrap();
    (
        min_x,
        min_y,
        (max_x - min_x) as u32,
        (max_y - min_y) as u32,
        enabled,
    )
}

fn create_pointer_probe(
    placements: &[OutputPlacement],
) -> (Connection, EventQueue<ProbeState>, ProbeState) {
    let connection = Connection::connect_to_env().expect("connect to Wayland");
    let mut queue = connection.new_event_queue();
    let queue_handle = queue.handle();
    connection.display().get_registry(&queue_handle, ());
    let mut state = ProbeState::default();
    let configured = Arc::new(AtomicU32::new(0));
    state.configured_surfaces = Some(Arc::clone(&configured));
    state.output_positions = placements
        .iter()
        .map(|output| (output.name.clone(), (output.x, output.y)))
        .collect();
    queue
        .roundtrip(&mut state)
        .expect("discover Wayland globals");
    queue.roundtrip(&mut state).expect("receive output names");

    let compositor = state.compositor.as_ref().expect("wl_compositor").clone();
    let shm = state.shm.as_ref().expect("wl_shm").clone();
    let seat = state.seat.as_ref().expect("wl_seat").clone();
    let shell = state.layer_shell.as_ref().expect("wlr layer shell").clone();
    let outputs: HashMap<String, wl_output::WlOutput> = state
        .outputs
        .iter()
        .filter_map(|output| {
            output
                .name
                .as_ref()
                .map(|name| (name.clone(), output.proxy.clone()))
        })
        .collect();

    let mut surfaces = Vec::new();
    for placement in placements {
        let output = outputs
            .get(&placement.name)
            .unwrap_or_else(|| panic!("Wayland output {} missing", placement.name));
        let surface = compositor.create_surface(&queue_handle, ());
        state
            .surface_names
            .insert(surface.id(), placement.name.clone());
        let layer = shell.get_layer_surface(
            &surface,
            Some(output),
            zwlr_layer_shell_v1::Layer::Overlay,
            format!("umbriel-mcp-pointer-smoke-{}", placement.name),
            &queue_handle,
            (),
        );
        layer.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();
        surfaces.push((surface, layer));
    }
    connection.flush().expect("flush layer surfaces");
    while configured.load(Ordering::SeqCst) < placements.len() as u32 {
        queue
            .blocking_dispatch(&mut state)
            .expect("layer configure");
    }

    let width = 1920;
    let height = 1080;
    let stride = width * 4;
    let size = (stride * height) as usize;
    let path = std::env::temp_dir().join(format!("umbriel-mcp-pointer-{}.shm", std::process::id()));
    let backing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create shared-memory buffer");
    let _ = std::fs::remove_file(path);
    backing
        .set_len(size as u64)
        .expect("size shared-memory buffer");
    let pool = shm.create_pool(backing.as_fd(), size as i32, &queue_handle, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wl_shm::Format::Argb8888,
        &queue_handle,
        (),
    );
    for (surface, _) in &surfaces {
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
    }
    seat.get_pointer(&queue_handle, ());
    connection.flush().expect("map pointer probes");
    queue.roundtrip(&mut state).expect("map pointer probes");
    (connection, queue, state)
}

#[test]
#[ignore = "requires a running Umbriel session with two side-by-side outputs"]
fn mcp_pointer_motion_is_exact_across_the_global_desktop_layout() {
    let (_, _, width, _, placements) = output_placements();
    assert!(
        placements.len() >= 2,
        "requires at least two enabled outputs"
    );
    assert!(
        width > 1920,
        "outputs must be arranged side by side for this check"
    );

    let (connection, mut queue, mut state) = create_pointer_probe(&placements);
    let mut mcp = McpProcess::start();
    for output in &placements {
        let target = (
            output.x + output.width as i32 / 2,
            output.y + output.height as i32 / 2,
        );
        state.last_global_position.lock().unwrap().take();
        mcp.move_pointer(target.0, target.1);
        let observed = wait_for_position(&connection, &mut queue, &mut state, target);
        assert_eq!(
            observed, target,
            "pointer should land at exact global coordinates on output {}",
            output.name
        );
    }

    let first = &placements[0];
    let last = placements.last().expect("second output");
    let from = (first.x + 300, first.y + 300);
    let to = (last.x + 400, last.y + 400);
    for (tool, arguments) in [
        ("mouse_click", json!({"button":"left"})),
        ("mouse_scroll", json!({"direction":"down","amount":1})),
        (
            "mouse_drag",
            json!({"from_x":from.0,"from_y":from.1,"to_x":to.0,"to_y":to.1,"button":"left"}),
        ),
    ] {
        let response = mcp.call_tool(tool, arguments);
        assert!(
            response.get("result").is_some(),
            "{tool} failed: {response}"
        );
    }
    let observed = wait_for_position(&connection, &mut queue, &mut state, to);
    assert_eq!(observed, to, "drag should end on the second output");
}

fn wait_for_position(
    connection: &Connection,
    queue: &mut EventQueue<ProbeState>,
    state: &mut ProbeState,
    expected: (i32, i32),
) -> (i32, i32) {
    connection.flush().expect("flush pointer observer");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        queue
            .dispatch_pending(state)
            .expect("dispatch pointer events");
        let observed = *state.last_global_position.lock().unwrap();
        if let Some(position) = observed {
            if position == expected || Instant::now() >= deadline {
                return position;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "no pointer event observed within 3 seconds"
        );
        let fd = connection.as_fd();
        let mut descriptors = [PollFd::new(&fd, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: remaining.as_secs() as i64,
            tv_nsec: i64::from(remaining.subsec_nanos()),
        };
        poll(&mut descriptors, Some(&timeout)).expect("poll pointer events");
        if descriptors[0].revents().contains(PollFlags::IN) {
            if let Some(read_guard) = queue.prepare_read() {
                read_guard.read().expect("read pointer events");
            }
        }
    }
}

fn wait_until_active(mcp: &mut McpProcess, window_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = mcp.call_tool("get_active_window", json!({}));
        let active = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if active.contains(window_id) {
            return;
        }
        assert!(Instant::now() < deadline, "window never became active");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_until_window_closes(mcp: &mut McpProcess, window_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = mcp.call_tool("list_windows", json!({}));
        let windows = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if !windows.contains(window_id) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "ctrl+d did not end the terminal's input, so it never reached the window as EOF"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn capture_terminal_output(title: &str, interact: impl FnOnce(&mut McpProcess)) -> String {
    let path = std::env::temp_dir().join(format!(
        "umbriel-mcp-input-{}-{title}.txt",
        std::process::id()
    ));
    let command = format!("kitty --title={title} -e sh -c 'cat > {}'", path.display());
    let mut mcp = McpProcess::start();
    let launched = mcp.call_tool("launch_app", json!({"command":command}));
    assert!(
        launched.get("result").is_some(),
        "launch_app failed: {launched}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let window_id = loop {
        let response = mcp.call_tool("list_windows", json!({}));
        let content = response["result"]["content"][0]["text"]
            .as_str()
            .expect("window list text");
        let windows: Value = serde_json::from_str(content).expect("window list JSON");
        if let Some(id) = windows
            .as_array()
            .into_iter()
            .flatten()
            .find(|window| window["title"].as_str() == Some(title))
            .and_then(|window| window["id"].as_str())
        {
            break id.to_string();
        }
        assert!(Instant::now() < deadline, "test terminal did not appear");
        std::thread::sleep(Duration::from_millis(100));
    };

    let focused = mcp.call_tool("focus_window", json!({"window_id":window_id}));
    assert!(
        focused.get("result").is_some(),
        "focus_window failed: {focused}"
    );
    wait_until_active(&mut mcp, &window_id);
    interact(&mut mcp);
    let eof = mcp.call_tool("key_press", json!({"keys":"ctrl+d"}));
    assert!(eof.get("result").is_some(), "ctrl+d failed: {eof}");

    wait_until_window_closes(&mut mcp, &window_id);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = String::new();
    let actual = loop {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if !current.is_empty() && current == previous {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "typed output never settled; last read {current:?}"
        );
        previous = current;
        std::thread::sleep(Duration::from_millis(300));
    };
    let _ = std::fs::remove_file(path);
    actual
}

fn assert_tool_succeeded(mcp: &mut McpProcess, tool: &str, arguments: Value) {
    let response = mcp.call_tool(tool, arguments);
    assert!(
        response.get("result").is_some(),
        "{tool} failed: {response}"
    );
}

#[test]
#[ignore = "requires a running Umbriel session and kitty"]
fn mcp_virtual_keyboard_types_unicode_and_key_chords() {
    let actual = capture_terminal_output("umbriel-mcp-input-smoke", |mcp| {
        assert_tool_succeeded(mcp, "type_text", json!({"text":"Umbriel MCP — Unicode ✓"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"Return"}));
    });
    assert_eq!(actual, "Umbriel MCP — Unicode ✓\n");
}

#[test]
#[ignore = "requires a running Umbriel session and kitty"]
fn mcp_virtual_keyboard_releases_modifiers_after_each_chord() {
    let actual = capture_terminal_output("umbriel-mcp-modifier-smoke", |mcp| {
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"shift+a"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"a"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"A"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"a"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"Return"}));
    });
    assert_eq!(actual, "AaAa\n");
}

#[test]
#[ignore = "requires a running Umbriel session and kitty"]
fn mcp_virtual_keyboard_types_more_distinct_characters_than_one_keymap_holds() {
    let many_distinct: String = (0..450u32)
        .map(|offset| char::from_u32(0x4E00 + offset).expect("valid CJK code point"))
        .collect();
    let text = format!("Hello, World! ÅÄÖ 日本語 😀 {many_distinct} end");
    let expected = format!("{text}\n");
    let actual = capture_terminal_output("umbriel-mcp-chunk-smoke", |mcp| {
        assert_tool_succeeded(mcp, "type_text", json!({"text":text}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"Return"}));
    });
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "requires a running Umbriel session and kitty"]
fn mcp_virtual_keyboard_types_multiline_text_and_tabs() {
    let actual = capture_terminal_output("umbriel-mcp-newline-smoke", |mcp| {
        assert_tool_succeeded(mcp, "type_text", json!({"text":"one\ntwo\r\nthree"}));
        assert_tool_succeeded(mcp, "key_press", json!({"keys":"Return"}));
    });
    assert_eq!(actual, "one\ntwo\nthree\n");
}

#[test]
#[ignore = "requires a running Umbriel session and kitty"]
fn mcp_virtual_keyboard_rejects_uncomposable_input_before_typing_anything() {
    let mut mcp = McpProcess::start();
    let bad_text = mcp.call_tool("type_text", json!({"text":"fine\u{7}bell"}));
    assert!(
        bad_text["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("control character")),
        "{bad_text}"
    );
    let bad_chord = mcp.call_tool("key_press", json!({"keys":"ctrl+nonsense"}));
    assert!(bad_chord.get("error").is_some(), "{bad_chord}");
}
