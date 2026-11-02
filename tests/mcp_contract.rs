use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct FakeUmbriel {
    path: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeUmbriel {
    fn start(replies: HashMap<String, Value>) -> Self {
        let path = std::env::temp_dir().join(format!(
            "umbriel-mcp-test-{}-{}.sock",
            std::process::id(),
            unique_suffix()
        ));
        let listener = UnixListener::bind(&path).expect("bind fake compositor socket");
        listener
            .set_nonblocking(true)
            .expect("set fake compositor socket nonblocking");

        let requests = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));
        let worker_requests = Arc::clone(&requests);
        let worker_running = Arc::clone(&running);
        let worker = thread::spawn(move || {
            while worker_running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => handle_fake_request(stream, &replies, &worker_requests),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            path,
            requests,
            running,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("fake request lock").clone()
    }
}

impl Drop for FakeUmbriel {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.path);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    )
}

fn handle_fake_request(
    stream: UnixStream,
    replies: &HashMap<String, Value>,
    requests: &Arc<Mutex<Vec<Value>>>,
) {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }

    let Ok(request) = serde_json::from_str::<Value>(&line) else {
        return;
    };
    requests
        .lock()
        .expect("fake request lock")
        .push(request.clone());

    let command = request
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let response = replies
        .get(command)
        .cloned()
        .unwrap_or_else(|| json!({"ok": []}));
    let mut stream = stream.into_inner();
    let _ = writeln!(stream, "{response}");
}

struct McpProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    approval_reply: Option<Value>,
    approval_questions: Vec<String>,
    approval_forms: Vec<Value>,
}

impl McpProcess {
    fn start(socket_path: &std::path::Path) -> Self {
        Self::start_with_args(socket_path, &[])
    }

    fn start_with_profile(socket_path: &std::path::Path, profile: &str) -> Self {
        Self::start_with_args(socket_path, &["--profile", profile])
    }

    fn start_with_args(socket_path: &std::path::Path, args: &[&str]) -> Self {
        Self::start_with_environment(socket_path, args, &[])
    }

    fn start_with_environment(
        socket_path: &std::path::Path,
        args: &[&str],
        environment: &[(&str, &std::ffi::OsStr)],
    ) -> Self {
        Self::start_as_client(socket_path, args, environment, json!({}), None)
    }

    fn start_as_client(
        socket_path: &std::path::Path,
        args: &[&str],
        environment: &[(&str, &std::ffi::OsStr)],
        capabilities: Value,
        approval_reply: Option<Value>,
    ) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
            .args(args)
            .envs(environment.iter().copied())
            .env("UMBRIEL_SOCKET", socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start MCP server binary");
        let input = child.stdin.take().expect("MCP server stdin");
        let output = BufReader::new(child.stdout.take().expect("MCP server stdout"));
        let mut process = Self {
            child,
            input,
            output,
            next_id: 1,
            approval_reply,
            approval_questions: Vec::new(),
            approval_forms: Vec::new(),
        };

        let initialized = process.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "clientInfo": { "name": "contract-test", "version": "1" }
            }),
        );
        assert_eq!(initialized["result"]["serverInfo"]["name"], "umbriel-mcp");
        process.notify("notifications/initialized", json!({}));
        process
    }

    fn notify(&mut self, method: &str, params: Value) {
        let message = json!({"jsonrpc":"2.0", "method":method, "params":params});
        writeln!(self.input, "{message}").expect("write MCP notification");
        self.input.flush().expect("flush MCP notification");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        writeln!(self.input, "{message}").expect("write MCP request");
        self.input.flush().expect("flush MCP request");

        loop {
            let mut line = String::new();
            self.output.read_line(&mut line).expect("read MCP response");
            assert!(!line.is_empty(), "MCP server closed stdout unexpectedly");
            let response: Value = serde_json::from_str(&line).expect("MCP response JSON");
            if response.get("method").is_some() {
                self.answer_server_request(&response);
                continue;
            }
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                return response;
            }
        }
    }

    fn answer_server_request(&mut self, request: &Value) {
        let Some(request_id) = request.get("id") else {
            return;
        };
        let result = if request["method"] == "elicitation/create" {
            self.approval_forms
                .push(request["params"]["requestedSchema"].clone());
            self.approval_questions.push(
                request["params"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            );
            self.approval_reply
                .clone()
                .unwrap_or_else(|| json!({"action": "cancel"}))
        } else {
            json!({})
        };
        let message = json!({"jsonrpc":"2.0", "id": request_id, "result": result});
        writeln!(self.input, "{message}").expect("answer server request");
        self.input.flush().expect("flush answer");
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name, "arguments":arguments}))
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.input.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_content(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .expect("tool result content")
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn canned_replies() -> HashMap<String, Value> {
    HashMap::from([
        (
            "windows".to_string(),
            json!({
                "ok": [
                    {
                        "id": "window-a",
                        "app_id": "kitty",
                        "title": "terminal",
                        "output": "HDMI-A-1",
                        "workspace": "HDMI-A-1:1",
                        "x": 0,
                        "y": 34,
                        "w": 960,
                        "h": 1022
                    },
                    {
                        "id": "window-b",
                        "app_id": "firefox",
                        "title": "docs",
                        "output": "DP-1",
                        "workspace": "DP-1:2",
                        "x": 1932,
                        "y": 34,
                        "w": 1896,
                        "h": 1022
                    }
                ]
            }),
        ),
        (
            "workspaces".to_string(),
            json!({
                "ok": [
                    { "id": "HDMI-A-1:1", "name": "1", "output": "HDMI-A-1", "focused": true },
                    { "id": "DP-1:2", "name": "2", "output": "DP-1", "focused": false }
                ]
            }),
        ),
        (
            "outputs".to_string(),
            json!({
                "ok": [
                    { "name": "HDMI-A-1", "position": { "x": 0, "y": 0 } },
                    { "name": "DP-1", "position": { "x": 1920, "y": 0 } }
                ]
            }),
        ),
    ])
}

#[test]
fn mcp_client_can_list_windows_workspaces_and_outputs() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let workspaces = server.request("tools/list", json!({}));
    let names: Vec<&str> = workspaces["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"list_windows"));
    assert!(names.contains(&"list_workspaces"));
    assert!(names.contains(&"list_outputs"));

    let windows = server.call_tool("list_windows", json!({}));
    let listed_windows: Value =
        serde_json::from_str(&text_content(&windows)).expect("windows JSON");
    assert_eq!(listed_windows.as_array().expect("windows array").len(), 2);
    assert_eq!(listed_windows[0]["app_id"], "kitty");

    let workspaces = server.call_tool("list_workspaces", json!({}));
    let listed_workspaces: Value =
        serde_json::from_str(&text_content(&workspaces)).expect("workspaces JSON");
    assert_eq!(listed_workspaces[1]["output"], "DP-1");

    let outputs = server.call_tool("list_outputs", json!({}));
    let listed_outputs: Value =
        serde_json::from_str(&text_content(&outputs)).expect("outputs JSON");
    assert_eq!(listed_outputs[1]["name"], "DP-1");

    let requested_commands: Vec<String> = fake
        .requests()
        .into_iter()
        .filter_map(|request| request["cmd"].as_str().map(str::to_owned))
        .collect();
    assert!(
        requested_commands
            .iter()
            .any(|command| command == "windows")
    );
    assert!(
        requested_commands
            .iter()
            .any(|command| command == "workspaces")
    );
    assert!(
        requested_commands
            .iter()
            .any(|command| command == "outputs")
    );
}

#[test]
fn mcp_client_can_filter_windows_by_output_and_workspace() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let response = server.call_tool("list_windows", json!({"output":"DP-1"}));
    let windows: Value =
        serde_json::from_str(&text_content(&response)).expect("filtered windows JSON");
    let windows = windows.as_array().expect("filtered windows array");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["id"], "window-b");

    let response = server.call_tool("list_windows", json!({"workspace":"HDMI-A-1:1"}));
    let windows: Value =
        serde_json::from_str(&text_content(&response)).expect("filtered windows JSON");
    let windows = windows.as_array().expect("filtered windows array");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["id"], "window-a");
}

#[test]
fn mcp_client_receives_compositor_errors_as_tool_errors() {
    let replies = HashMap::from([(
        "windows".to_string(),
        json!({"err":"window query denied by fake compositor"}),
    )]);
    let fake = FakeUmbriel::start(replies);
    let mut server = McpProcess::start(&fake.path);

    let response = server.call_tool("list_windows", json!({}));
    let serialized = response.to_string();
    assert_eq!(
        response["error"]["code"], -32603,
        "expected MCP server error: {serialized}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("window query denied by fake compositor")),
        "expected compositor error in MCP response, got {serialized}"
    );
}

const ALL_PROFILES: [&str; 3] = ["read-only", "no-exec", "full"];

fn compositor_actions(fake: &FakeUmbriel) -> Vec<String> {
    fake.requests()
        .into_iter()
        .filter(|request| request["cmd"] == "msg")
        .filter_map(|request| request["arg"].as_str().map(str::to_owned))
        .collect()
}

fn error_message(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error response, got {response}"))
        .to_owned()
}

fn assert_refused_by_profile(response: &Value, profile: &str) {
    let message = error_message(response);
    assert!(
        message.contains("profile") && message.contains(profile),
        "refusal should name the profile `{profile}`, got: {message}"
    );
}

fn assert_not_refused_by_profile(response: &Value) {
    if let Some(message) = response["error"]["message"].as_str() {
        assert!(
            !message.contains("profile"),
            "call should have passed the profile check, got: {message}"
        );
    }
}

fn advertised_tool_names(server: &mut McpProcess) -> Vec<String> {
    let response = server.request("tools/list", json!({}));
    let mut names: Vec<String> = response["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

#[test]
fn every_profile_still_advertises_every_tool() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut full = McpProcess::start(&fake.path);
    let expected = advertised_tool_names(&mut full);
    assert!(expected.contains(&"launch_app".to_string()));

    for profile in ALL_PROFILES {
        let mut server = McpProcess::start_with_profile(&fake.path, profile);
        assert_eq!(
            advertised_tool_names(&mut server),
            expected,
            "profile {profile} must not hide tools"
        );
    }
}

#[test]
fn profile_defaults_to_full() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let response = server.call_tool("launch_app", json!({"command":"kitty"}));

    assert_not_refused_by_profile(&response);
    assert_eq!(compositor_actions(&fake), vec!["spawn:kitty"]);
}

#[test]
fn full_profile_permits_launching_and_spawn_actions() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "full");

    server.call_tool("launch_app", json!({"command":"kitty"}));
    server.call_tool("run_action", json!({"action":"spawn:foot"}));
    server.call_tool("run_action", json!({"action":"overview-toggle"}));

    assert_eq!(
        compositor_actions(&fake),
        vec!["spawn:kitty", "spawn:foot", "overview-toggle"]
    );
}

#[test]
fn no_exec_profile_refuses_the_launch_tool() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "no-exec");

    let response = server.call_tool("launch_app", json!({"command":"kitty"}));

    assert_refused_by_profile(&response, "no-exec");
    assert_eq!(compositor_actions(&fake), Vec::<String>::new());
}

#[test]
fn no_exec_profile_refuses_every_spawn_form_through_run_action() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "no-exec");

    for action in [
        "spawn:kitty",
        "SPAWN:kitty",
        "  spawn:kitty",
        "\tspawn:kitty",
        "spawn kitty",
        "spawn",
        "Spawn:sh -c 'id'",
    ] {
        let response = server.call_tool("run_action", json!({"action":action}));
        assert_refused_by_profile(&response, "no-exec");
    }
    assert_eq!(
        compositor_actions(&fake),
        Vec::<String>::new(),
        "no spawn action may reach the compositor"
    );
}

#[test]
fn no_exec_profile_permits_window_and_workspace_control() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "no-exec");

    server.call_tool("switch_workspace", json!({"workspace":"2","output":"DP-1"}));
    server.call_tool("toggle_fullscreen", json!({}));
    server.call_tool("close_window", json!({"window_id":"window-a"}));
    server.call_tool("run_action", json!({"action":"overview-toggle"}));

    assert_eq!(
        compositor_actions(&fake),
        vec![
            "workspace-switch:2/DP-1",
            "window-toggle-fullscreen",
            "window-close:window-a",
            "overview-toggle"
        ]
    );
}

#[test]
fn no_exec_profile_permits_input_tools_past_the_profile_check() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "no-exec");

    let key = server.call_tool("key_press", json!({"keys":"ctrl"}));
    let click = server.call_tool("mouse_click", json!({"button":"not-a-button"}));

    assert_not_refused_by_profile(&key);
    assert_not_refused_by_profile(&click);
    assert!(
        error_message(&key).contains("key"),
        "expected a chord error"
    );
    assert!(
        error_message(&click).contains("button"),
        "expected a button error"
    );
}

#[test]
fn read_only_profile_permits_queries() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "read-only");

    for tool in [
        "list_windows",
        "list_workspaces",
        "list_outputs",
        "get_active_window",
        "list_layers",
    ] {
        let response = server.call_tool(tool, json!({}));
        assert!(
            response.get("result").is_some(),
            "{tool} should work read-only: {response}"
        );
    }
}

#[test]
fn read_only_profile_refuses_every_state_changing_tool() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start_with_profile(&fake.path, "read-only");

    let refused_calls = [
        ("switch_workspace", json!({"workspace":"2"})),
        ("focus_window", json!({"window_id":"window-a"})),
        ("close_window", json!({})),
        ("kill_window", json!({})),
        ("move_window_to_workspace", json!({"workspace":"2"})),
        ("toggle_fullscreen", json!({})),
        ("toggle_floating", json!({})),
        ("resize_window", json!({"edge":"left","delta":0.1})),
        ("set_workspace_layout", json!({"mode":"dwindle"})),
        ("toggle_overview", json!({})),
        ("launch_app", json!({"command":"kitty"})),
        ("run_action", json!({"action":"overview-toggle"})),
        ("run_action", json!({"action":"spawn:kitty"})),
        ("mouse_move", json!({"x":10,"y":10})),
        ("mouse_click", json!({})),
        ("mouse_scroll", json!({})),
        (
            "mouse_drag",
            json!({"from_x":1,"from_y":1,"to_x":2,"to_y":2}),
        ),
        ("type_text", json!({"text":"hello"})),
        ("key_press", json!({"keys":"Return"})),
        ("click_text", json!({"target":"File"})),
        ("clipboard_write", json!({"text":"hello"})),
    ];
    for (tool, arguments) in refused_calls {
        let response = server.call_tool(tool, arguments);
        assert_refused_by_profile(&response, "read-only");
    }
    assert_eq!(
        compositor_actions(&fake),
        Vec::<String>::new(),
        "no state-changing request may reach the compositor"
    );
}

#[test]
fn an_unknown_profile_is_rejected_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
        .args(["--profile", "bogus"])
        .stdin(Stdio::null())
        .output()
        .expect("run MCP server binary");

    assert!(
        !output.status.success(),
        "unknown profile must fail startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for allowed in ALL_PROFILES {
        assert!(
            stderr.contains(allowed),
            "stderr should list `{allowed}`: {stderr}"
        );
    }
}

const OCR_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ocr-ui-wide.png"
);

fn fake_tools_directory(
    grim_script: Option<&str>,
    tesseract_script: Option<&str>,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let directory = std::env::temp_dir().join(format!(
        "umbriel-ocr-tools-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&directory).expect("create fake tools directory");
    for (name, script) in [("grim", grim_script), ("tesseract", tesseract_script)] {
        if let Some(script) = script {
            let path = directory.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake tool");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }
    directory
}

fn grim_that_captures_the_fixture() -> String {
    format!("exec /usr/bin/cat '{OCR_FIXTURE}'")
}

fn start_with_tools(fake: &FakeUmbriel, tools: &std::path::Path, search_path: &str) -> McpProcess {
    let path = format!("{}:{search_path}", tools.display());
    McpProcess::start_with_environment(&fake.path, &[], &[("PATH", path.as_ref())])
}

#[test]
fn ocr_tools_read_a_native_resolution_capture() {
    let fake = FakeUmbriel::start(canned_replies());
    let tools = fake_tools_directory(Some(&grim_that_captures_the_fixture()), None);
    let mut server = start_with_tools(&fake, &tools, "/usr/bin");

    let screenshot = server.call_tool("screenshot_with_ocr", json!({}));
    let found = server.call_tool("find_text_on_screen", json!({"target":"Cancel"}));
    let phrase = server.call_tool("find_text_on_screen", json!({"target":"Open File"}));

    let read = text_content(&screenshot);
    for expected in ["Settings", "Save", "Cancel", "quick brown fox 12345"] {
        assert!(
            read.contains(expected),
            "OCR text missing {expected:?}:\n{read}"
        );
    }
    let located = text_content(&found);
    assert!(
        located.contains("Cancel") && located.contains("at screen (3"),
        "{located}"
    );
    assert!(
        text_content(&phrase).contains("\"Open File\""),
        "{}",
        text_content(&phrase)
    );
}

#[test]
fn ocr_tools_name_the_missing_program() {
    let fake = FakeUmbriel::start(canned_replies());
    let tools = fake_tools_directory(Some(&grim_that_captures_the_fixture()), None);
    let mut server = start_with_tools(&fake, &tools, "");

    let response = server.call_tool("screenshot_with_ocr", json!({}));

    let message = error_message(&response);
    assert!(message.contains("tesseract"), "{message}");
    assert!(message.contains("doctor"), "{message}");
}

#[test]
fn ocr_tools_name_the_missing_language_package() {
    let fake = FakeUmbriel::start(canned_replies());
    let tools = fake_tools_directory(
        Some(&grim_that_captures_the_fixture()),
        Some("echo 'Error opening data file /usr/share/tessdata/eng.traineddata' >&2; exit 1"),
    );
    let mut server = start_with_tools(&fake, &tools, "/usr/bin");

    let response = server.call_tool("find_text_on_screen", json!({"target":"Save"}));

    let message = error_message(&response);
    assert!(message.contains("tesseract-data-eng"), "{message}");
    assert!(message.contains("tesseract-ocr-eng"), "{message}");
}

#[test]
fn concurrent_ocr_calls_do_not_trample_each_other() {
    let fake = FakeUmbriel::start(canned_replies());
    let tools = fake_tools_directory(Some(&grim_that_captures_the_fixture()), None);
    let search_path = "/usr/bin";
    let mut handles = Vec::new();
    for _ in 0..3 {
        let socket = fake.path.clone();
        let tools = tools.clone();
        handles.push(std::thread::spawn(move || {
            let path = format!("{}:{search_path}", tools.display());
            let mut server =
                McpProcess::start_with_environment(&socket, &[], &[("PATH", path.as_ref())]);
            text_content(&server.call_tool("screenshot_with_ocr", json!({})))
        }));
    }
    for handle in handles {
        assert!(handle.join().expect("ocr thread").contains("Cancel"));
    }
}

fn windows_on_two_outputs() -> HashMap<String, Value> {
    HashMap::from([(
        "windows".to_string(),
        json!({
            "ok": [
                { "id": "left-4", "title": "Figma", "output": "HDMI-A-1", "workspace": "HDMI-A-1:4" },
                { "id": "right-4", "title": "notes", "output": "DP-1", "workspace": "DP-1:4" },
                { "id": "right-1", "title": "shell", "output": "DP-1", "workspace": "DP-1:1" },
                { "id": "named", "title": "chat", "output": "DP-1", "workspace": "DP-1:web" }
            ]
        }),
    )])
}

fn listed_window_ids(server: &mut McpProcess, arguments: Value) -> Vec<String> {
    let response = server.call_tool("list_windows", arguments);
    let windows: Value =
        serde_json::from_str(&text_content(&response)).expect("filtered windows JSON");
    windows
        .as_array()
        .expect("filtered windows array")
        .iter()
        .filter_map(|window| window["id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_bare_workspace_number_lists_that_workspace_on_every_output() {
    let fake = FakeUmbriel::start(windows_on_two_outputs());
    let mut server = McpProcess::start(&fake.path);

    assert_eq!(
        listed_window_ids(&mut server, json!({"workspace":"4"})),
        vec!["left-4", "right-4"]
    );
    assert_eq!(
        listed_window_ids(&mut server, json!({"workspace":"4","output":"DP-1"})),
        vec!["right-4"]
    );
    assert_eq!(
        listed_window_ids(&mut server, json!({"workspace":"DP-1:4"})),
        vec!["right-4"]
    );
    assert_eq!(
        listed_window_ids(&mut server, json!({"workspace":"web"})),
        vec!["named"]
    );
    assert!(listed_window_ids(&mut server, json!({"workspace":"14"})).is_empty());
}

#[test]
fn the_server_tells_clients_to_use_it_for_this_desktop_instead_of_other_compositor_tools() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let initialized = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "instructions-check", "version": "1" }
        }),
    );
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let tools = server.request("tools/list", json!({}));
    let list_windows_description = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "list_windows")
        .and_then(|tool| tool["description"].as_str())
        .expect("list_windows description")
        .to_lowercase();

    assert!(instructions.contains("this desktop"), "{instructions}");
    for other_tool in ["hyprctl", "swaymsg", "wmctrl"] {
        assert!(instructions.contains(other_tool), "{instructions}");
    }
    assert!(
        list_windows_description.contains("workspace 4"),
        "{list_windows_description}"
    );
}

fn one_visible_and_one_hidden_window() -> HashMap<String, Value> {
    HashMap::from([(
        "windows".to_string(),
        json!({
            "ok": [
                { "id": "shown", "title": "editor", "output": "DP-1", "workspace": "DP-1:1",
                  "visible": true, "x": 1932, "y": 46, "w": 900, "h": 500 },
                { "id": "hidden", "title": "Figma", "output": "HDMI-A-1", "workspace": "HDMI-A-1:4",
                  "visible": false, "x": 12, "y": 46, "w": 1896, "h": 1022 }
            ]
        }),
    )])
}

fn recording_grim(tools: &std::path::Path, supports_toplevel_capture: bool) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let record = tools.join("grim.args");
    let help = if supports_toplevel_capture {
        "  -T <identifier> Set the identifier of a foreign toplevel handle to capture."
    } else {
        "  -g <geometry>   Set the region to capture."
    };
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"-h\" ]; then echo 'Usage: grim [options...] [output-file]'; echo '{help}'; exit 0; fi\nprintf '%s\\n' \"$@\" >> '{}'\nif [ \"$5\" = \"-T\" ] && [ \"$6\" != \"shown\" ] && [ \"$6\" != \"hidden\" ]; then echo 'cannot find toplevel' >&2; exit 1; fi\nexec /usr/bin/cat '{OCR_FIXTURE}'\n",
        record.display()
    );
    let path = tools.join("grim");
    std::fs::write(&path, script).expect("write fake grim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    record
}

fn grim_arguments(record: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn start_with_grim(
    supports_toplevel_capture: bool,
) -> (FakeUmbriel, McpProcess, std::path::PathBuf) {
    let fake = FakeUmbriel::start(one_visible_and_one_hidden_window());
    let tools = fake_tools_directory(None, None);
    let record = recording_grim(&tools, supports_toplevel_capture);
    let server = start_with_tools(&fake, &tools, "/usr/bin");
    (fake, server, record)
}

#[test]
fn a_window_on_a_hidden_workspace_is_captured_by_its_id_without_switching() {
    let (fake, mut server, record) = start_with_grim(true);

    let response = server.call_tool("screenshot", json!({"window_id":"hidden"}));

    assert!(response.get("result").is_some(), "{response}");
    let arguments = grim_arguments(&record);
    assert!(
        arguments.windows(2).any(|pair| pair == ["-T", "hidden"]),
        "{arguments:?}"
    );
    assert!(!arguments.contains(&"-g".to_owned()), "{arguments:?}");
    assert_eq!(
        compositor_actions(&fake),
        Vec::<String>::new(),
        "capture must not switch workspaces"
    );
    let note = text_content(&response);
    assert!(note.contains("not currently shown"), "{note}");
}

#[test]
fn a_visible_window_capture_reports_screen_coordinates_from_its_position() {
    let (_fake, mut server, _record) = start_with_grim(true);

    let response = server.call_tool("screenshot", json!({"window_id":"shown"}));

    let note = text_content(&response);
    assert!(note.contains("absolute (1932, 46)"), "{note}");
    assert!(!note.contains("not currently shown"), "{note}");
}

#[test]
fn an_older_grim_falls_back_to_the_region_of_a_visible_window() {
    let (_fake, mut server, record) = start_with_grim(false);

    let response = server.call_tool("screenshot", json!({"window_id":"shown"}));

    assert!(response.get("result").is_some(), "{response}");
    let arguments = grim_arguments(&record);
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["-g", "1932,46 900x500"]),
        "{arguments:?}"
    );
}

#[test]
fn an_older_grim_refuses_a_hidden_window_instead_of_capturing_whatever_covers_it() {
    let (_fake, mut server, record) = start_with_grim(false);

    let response = server.call_tool("screenshot", json!({"window_id":"hidden"}));

    let message = error_message(&response);
    assert!(message.contains("grim 1.5"), "{message}");
    assert!(message.contains("not currently shown"), "{message}");
    assert!(
        grim_arguments(&record).is_empty(),
        "nothing should be captured"
    );
}

#[test]
fn an_unknown_window_id_is_reported_as_such() {
    let (_fake, mut server, _record) = start_with_grim(true);

    let response = server.call_tool("screenshot", json!({"window_id":"nope"}));

    assert!(
        error_message(&response).contains("no window with id nope"),
        "{response}"
    );
}

#[test]
fn ocr_can_read_a_window_on_a_hidden_workspace() {
    let (_fake, mut server, record) = start_with_grim(true);

    let response = server.call_tool("screenshot_with_ocr", json!({"window_id":"hidden"}));

    assert!(
        text_content(&response).contains("Cancel"),
        "{}",
        text_content(&response)
    );
    assert!(grim_arguments(&record).contains(&"-T".to_owned()));
}

fn fake_umbriel_cli(tools: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
case "$*" in
  "--version") echo "umbriel 9.9.9 (fake)" ;;
  "msg --help") cat <<'HELP'
Usage: umbriel msg <action> [args...]

Available actions, grouped as the cheatsheet groups them:

Apps
  spawn:<cmd>  Run a command with a launch activation token

Workspaces
  workspace-switch:<selector>  Switch to a workspace
  workspace-next               Switch to the next workspace
HELP
  ;;
  "config schema --json") echo '{"version":"9.9.9","options":[{"path":"layout","type":"table"},{"path":"layout.gap","type":"int","min":0,"max":500,"default":8},{"path":"appearance.blur.enabled","type":"bool","default":true},{"path":"general.autostart","type":"string_array","default":[]},{"path":"hot_corners.top_left.action","type":"string","format":"action"},{"path":"output.<name>.enabled","type":"bool","default":true},{"path":"input.keyboard.layout","type":"string","default":""},{"path":"keybinds.<name>","type":"string_or_table","format":"action"},{"path":"window_rule","type":"array_of_tables"}]}' ;;
  "config validate -c "*)
    if grep -q "no-such-action" "$4"; then
      printf '12:00:00.000 [\033[33mWRN\033[0m] [config] %s:3:1: ignoring keybind\n' "$4"
      echo "warning: $4:3:1: ignoring keybind 'Mod+X' (unknown action 'no-such-action')"
      echo "configuration invalid"
      exit 1
    fi
    echo "config: ok ($4)" ;;
  "config validate") echo "config: ok (fake)" ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;
    let path = tools.join("umbriel");
    std::fs::write(&path, script).expect("write fake umbriel");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn compositor_state_replies() -> HashMap<String, Value> {
    HashMap::from([
        (
            "keyboard-layouts".to_string(),
            json!({"ok": {"current_index": 0, "names": ["English (US)", "Bangla"]}}),
        ),
        ("submap".to_string(), json!({"ok": null})),
        ("color".to_string(), json!({"ok": {"color_manager": true}})),
        ("tearing".to_string(), json!({"ok": {"outputs": []}})),
    ])
}

fn start_read_only_with_umbriel_cli(home: &std::path::Path) -> (FakeUmbriel, McpProcess) {
    start_with_umbriel_cli(home, "read-only")
}

fn start_with_umbriel_cli(home: &std::path::Path, profile: &str) -> (FakeUmbriel, McpProcess) {
    let fake = FakeUmbriel::start(compositor_state_replies());
    let tools = fake_tools_directory(None, None);
    fake_umbriel_cli(&tools);
    let path = format!("{}:/usr/bin", tools.display());
    let server = McpProcess::start_with_environment(
        &fake.path,
        &["--profile", profile],
        &[
            ("PATH", path.as_ref()),
            ("HOME", home.as_os_str()),
            ("XDG_CONFIG_HOME", std::ffi::OsStr::new("")),
        ],
    );
    (fake, server)
}

fn home_with_config(config: &str) -> std::path::PathBuf {
    let home = fake_tools_directory(None, None);
    let directory = home.join(".config/umbriel");
    std::fs::create_dir_all(&directory).expect("create config dir");
    std::fs::write(directory.join("config.toml"), config).expect("write config");
    home
}

#[test]
fn umbriel_info_reports_version_and_live_state() {
    let home = home_with_config("[general]\n");
    let (_fake, mut server) = start_read_only_with_umbriel_cli(&home);

    let info: Value =
        serde_json::from_str(&text_content(&server.call_tool("umbriel_info", json!({})))).unwrap();

    assert_eq!(info["installed_version"], "umbriel 9.9.9 (fake)");
    assert_eq!(info["keyboard_layouts"]["names"][1], "Bangla");
    assert!(
        info["reference"]
            .as_str()
            .unwrap()
            .contains("docs.noctalia.dev")
    );
}

#[test]
fn list_actions_shows_runnable_actions_and_filters_them() {
    let home = home_with_config("[general]\n");
    let (_fake, mut server) = start_read_only_with_umbriel_cli(&home);

    let all = text_content(&server.call_tool("list_actions", json!({})));
    let workspace = text_content(&server.call_tool("list_actions", json!({"filter":"workspace"})));
    let none = text_content(&server.call_tool("list_actions", json!({"filter":"teleport"})));

    assert!(all.contains("3 of 3 actions"), "{all}");
    assert!(all.contains("spawn:<cmd>"), "{all}");
    assert!(workspace.contains("2 of 3 actions"), "{workspace}");
    assert!(!workspace.contains("spawn"), "{workspace}");
    assert!(none.contains("no action matches"), "{none}");
}

#[test]
fn config_options_returns_exact_keys_with_defaults() {
    let home = home_with_config("[general]\n");
    let (_fake, mut server) = start_read_only_with_umbriel_cli(&home);

    let found: Value = serde_json::from_str(&text_content(
        &server.call_tool("config_options", json!({"query":"gap"})),
    ))
    .unwrap();

    assert_eq!(found["options"][0]["path"], "layout.gap");
    assert_eq!(found["options"][0]["default"], 8);
    assert_eq!(found["total_matches"], 1);
}

#[test]
fn read_config_returns_a_section_with_its_validation() {
    let home = home_with_config(
        "[general]\nautostart = []\n\n[keybinds]\n\"Mod+T\" = { action = \"spawn:kitty\" }\n\n[layout]\ngap = 4\n",
    );
    let (_fake, mut server) = start_read_only_with_umbriel_cli(&home);

    let keybinds = text_content(&server.call_tool("read_config", json!({"section":"keybinds"})));
    let missing = server.call_tool("read_config", json!({"section":"hot_corners"}));

    assert!(
        keybinds.contains("\"Mod+T\" = { action = \"spawn:kitty\" }"),
        "{keybinds}"
    );
    assert!(!keybinds.contains("gap = 4"), "{keybinds}");
    assert!(
        keybinds.contains("Validation: config: ok (fake)"),
        "{keybinds}"
    );
    assert!(
        keybinds.contains(".config/umbriel/config.toml"),
        "{keybinds}"
    );
    assert!(
        error_message(&missing).contains("no [hot_corners] section"),
        "{missing}"
    );
}

#[test]
fn the_instructions_point_umbriel_questions_at_the_knowledge_tools() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let initialized = server.request(
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"i","version":"1"}}),
    );
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();

    for tool in [
        "umbriel_info",
        "list_actions",
        "config_options",
        "read_config",
    ] {
        assert!(instructions.contains(tool), "{instructions}");
    }
}

const KEYBIND_CONFIG: &str = "# my config\n[general]\nautostart = []\n\n[keybinds]\n\"Mod+T\" = { action = \"spawn:kitty\", repeat = false }\n\"Mod+Q\" = \"window-close\"\n\n[layout]\ngap = 4\n";

fn config_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config/umbriel/config.toml")
}

fn files_beside_config(home: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home.join(".config/umbriel"))
        .expect("config dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn set_keybind_adds_a_validated_binding_and_backs_up_the_old_config() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let response = call_and_approve(
        &mut server,
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-toggle-fullscreen"}),
    );

    let report = text_content(&response);
    let config = std::fs::read_to_string(config_file(&home)).unwrap();
    assert!(
        config.contains("\"Mod+F\" = \"window-toggle-fullscreen\""),
        "{config}"
    );
    assert!(config.starts_with("# my config"), "{config}");
    assert!(
        report.contains("+ \"Mod+F\" = \"window-toggle-fullscreen\""),
        "{report}"
    );
    let backups: Vec<String> = files_beside_config(&home)
        .into_iter()
        .filter(|name| name.starts_with("config.toml.bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "{:?}", files_beside_config(&home));
    let backup = std::fs::read_to_string(home.join(".config/umbriel").join(&backups[0])).unwrap();
    assert_eq!(backup, KEYBIND_CONFIG);
    assert!(report.contains(&backups[0]), "{report}");
}

#[test]
fn rebinding_reports_what_the_chord_did_before() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let report = text_content(&call_and_approve(
        &mut server,
        "set_keybind",
        json!({"chord":"mod+q","action":"window-kill"}),
    ));

    assert!(
        report.contains("Applied: Bind Mod+Q to window-kill (was window-close)"),
        "{report}"
    );
    let config = std::fs::read_to_string(config_file(&home)).unwrap();
    assert_eq!(config.matches("Mod+Q").count(), 1, "{config}");
}

#[test]
fn a_binding_umbriel_rejects_is_never_written() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let response = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+X","action":"no-such-action"}),
    );

    let message = error_message(&response);
    assert!(message.contains("rejected the change"), "{message}");
    assert!(
        message.contains("unknown action 'no-such-action'"),
        "{message}"
    );
    assert!(
        !message.contains('\u{1b}'),
        "terminal colour codes leaked: {message:?}"
    );
    assert!(
        !message.contains(".umbriel-mcp-"),
        "temporary file name leaked: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(config_file(&home)).unwrap(),
        KEYBIND_CONFIG
    );
    assert_eq!(files_beside_config(&home), vec!["config.toml"]);
}

#[test]
fn remove_keybind_drops_the_binding_and_reports_it() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let report = text_content(&call_and_approve(
        &mut server,
        "remove_keybind",
        json!({"chord":"Mod+T"}),
    ));
    let missing = call_and_approve(&mut server, "remove_keybind", json!({"chord":"Mod+Z"}));

    assert!(
        report.contains("Applied: Remove the binding for Mod+T (was spawn:kitty)"),
        "{report}"
    );
    assert!(
        !std::fs::read_to_string(config_file(&home))
            .unwrap()
            .contains("Mod+T")
    );
    assert!(
        error_message(&missing).contains("Mod+Z is not bound"),
        "{missing}"
    );
}

#[test]
fn no_exec_may_bind_ordinary_actions_but_not_programs() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "no-exec");

    let ordinary = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-toggle-fullscreen"}),
    );
    let program = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+B","action":"SPAWN:sh -c id"}),
    );

    assert!(ordinary.get("result").is_some(), "{ordinary}");
    assert_refused_by_profile(&program, "no-exec");
    assert!(
        !std::fs::read_to_string(config_file(&home))
            .unwrap()
            .contains("Mod+B")
    );
}

#[test]
fn read_only_may_not_change_keybinds() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "read-only");

    let set = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-close"}),
    );
    let remove = server.call_tool("remove_keybind", json!({"chord":"Mod+T"}));

    assert_refused_by_profile(&set, "read-only");
    assert_refused_by_profile(&remove, "read-only");
    assert_eq!(
        std::fs::read_to_string(config_file(&home)).unwrap(),
        KEYBIND_CONFIG
    );
}

#[test]
fn a_symlinked_config_stays_a_symlink_and_its_target_is_edited() {
    let home = home_with_config("");
    let dotfiles = home.join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    std::fs::write(dotfiles.join("umbriel.toml"), KEYBIND_CONFIG).unwrap();
    std::fs::remove_file(config_file(&home)).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("umbriel.toml"), config_file(&home)).unwrap();
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let response = call_and_approve(
        &mut server,
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-close"}),
    );

    assert!(response.get("result").is_some(), "{response}");
    assert!(
        std::fs::symlink_metadata(config_file(&home))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::read_to_string(dotfiles.join("umbriel.toml"))
            .unwrap()
            .contains("Mod+F")
    );
}

#[test]
fn a_chord_owned_by_an_included_file_is_left_alone() {
    let home = home_with_config(
        "[include]\nfiles = [\"binds.toml\"]\n\n[keybinds]\n\"Mod+Q\" = \"window-close\"\n",
    );
    std::fs::write(
        home.join(".config/umbriel/binds.toml"),
        "[keybinds]\n\"Mod+T\" = \"spawn:kitty\"\n",
    )
    .unwrap();
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let response = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+T","action":"spawn:foot"}),
    );

    let message = error_message(&response);
    assert!(message.contains("binds.toml"), "{message}");
    assert!(
        !std::fs::read_to_string(config_file(&home))
            .unwrap()
            .contains("Mod+T")
    );
}

#[test]
fn without_a_user_config_nothing_is_created_and_the_user_is_told_what_to_do() {
    let home = fake_tools_directory(None, None);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    let response = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-close"}),
    );

    let message = error_message(&response);
    assert!(message.contains("no user config"), "{message}");
    assert!(
        message.contains("/usr/share/umbriel/config.toml"),
        "{message}"
    );
    assert!(!config_file(&home).exists());
}

#[test]
fn quick_successive_edits_each_keep_their_own_backup() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_with_umbriel_cli(&home, "full");

    for (chord, action) in [
        ("Mod+F", "window-close"),
        ("Mod+G", "window-kill"),
        ("Mod+H", "window-close"),
    ] {
        let response = call_and_approve(
            &mut server,
            "set_keybind",
            json!({"chord":chord,"action":action}),
        );
        assert!(response.get("result").is_some(), "{response}");
    }

    let directory = home.join(".config/umbriel");
    let backups: Vec<String> = files_beside_config(&home)
        .into_iter()
        .filter(|name| name.starts_with("config.toml.bak-"))
        .collect();
    assert_eq!(backups.len(), 3, "{backups:?}");
    let contents: Vec<String> = backups
        .iter()
        .map(|name| std::fs::read_to_string(directory.join(name)).unwrap())
        .collect();
    assert!(
        contents.contains(&KEYBIND_CONFIG.to_owned()),
        "the original config must survive"
    );
}

#[test]
fn the_server_reports_the_crate_version() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let initialized = server.request(
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"v","version":"1"}}),
    );

    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

fn confirmation_code(response: &Value) -> Option<String> {
    let text = response["result"]["content"][0]["text"].as_str()?;
    let after = text.split("confirmation \"").nth(1)?;
    Some(after.split('"').next()?.to_owned())
}

fn call_and_approve(server: &mut McpProcess, tool: &str, arguments: Value) -> Value {
    let previewed = server.call_tool(tool, arguments);
    match confirmation_code(&previewed) {
        Some(code) => server.call_tool("apply_config_change", json!({"confirmation": code})),
        None => previewed,
    }
}

fn start_editor(
    home: &std::path::Path,
    profile: &str,
    outputs: Value,
    approval_reply: Option<Value>,
) -> (FakeUmbriel, McpProcess) {
    let mut replies = compositor_state_replies();
    replies.insert("outputs".to_string(), json!({ "ok": outputs }));
    let fake = FakeUmbriel::start(replies);
    let tools = fake_tools_directory(None, None);
    fake_umbriel_cli(&tools);
    let path = format!("{}:/usr/bin", tools.display());
    let capabilities = if approval_reply.is_some() {
        json!({"elicitation": {}})
    } else {
        json!({})
    };
    let server = McpProcess::start_as_client(
        &fake.path,
        &["--profile", profile],
        &[
            ("PATH", path.as_ref()),
            ("HOME", home.as_os_str()),
            ("XDG_CONFIG_HOME", std::ffi::OsStr::new("")),
        ],
        capabilities,
        approval_reply,
    );
    (fake, server)
}

fn two_outputs() -> Value {
    json!([{"name": "HDMI-A-1", "enabled": true}, {"name": "DP-1", "enabled": true}])
}

fn current_config(home: &std::path::Path) -> String {
    std::fs::read_to_string(config_file(home)).unwrap()
}

#[test]
fn without_a_dialog_nothing_is_written_until_the_confirmation_code_is_used() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let previewed = server.call_tool("set_option", json!({"key":"layout.gap","value":12}));

    let preview = text_content(&previewed);
    assert!(
        preview.contains("Nothing has been written yet"),
        "{preview}"
    );
    assert!(
        preview.contains("Set layout.gap to 12 (was 4)"),
        "{preview}"
    );
    assert!(preview.contains("+ gap = 12"), "{preview}");
    assert_eq!(current_config(&home), KEYBIND_CONFIG);
    assert_eq!(files_beside_config(&home), vec!["config.toml"]);

    let code = confirmation_code(&previewed).expect("confirmation code");
    let applied = server.call_tool("apply_config_change", json!({"confirmation": code}));
    assert!(
        text_content(&applied).contains("Applied: Set layout.gap to 12"),
        "{applied}"
    );
    assert!(current_config(&home).contains("gap = 12"));

    let reused = server.call_tool("apply_config_change", json!({"confirmation": code}));
    assert!(
        error_message(&reused).contains("no change is waiting"),
        "{reused}"
    );
    let invented = server.call_tool(
        "apply_config_change",
        json!({"confirmation": "123456789abc"}),
    );
    assert!(
        error_message(&invented).contains("no change is waiting"),
        "{invented}"
    );
}

#[test]
fn a_change_previewed_against_an_older_file_is_not_written() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let previewed = server.call_tool("set_option", json!({"key":"layout.gap","value":12}));
    std::fs::write(
        config_file(&home),
        format!("{KEYBIND_CONFIG}# edited by hand\n"),
    )
    .unwrap();
    let applied = server.call_tool(
        "apply_config_change",
        json!({"confirmation": confirmation_code(&previewed).unwrap()}),
    );

    assert!(
        error_message(&applied).contains("changed after this change was previewed"),
        "{applied}"
    );
    assert!(!current_config(&home).contains("gap = 12"));
}

#[test]
fn a_client_dialog_writes_only_when_the_user_approves() {
    let home = home_with_config(KEYBIND_CONFIG);
    let approve = json!({"action": "accept", "content": {"apply": true}});
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), Some(approve));

    let response = server.call_tool(
        "set_keybind",
        json!({"chord":"Mod+F","action":"window-close"}),
    );

    assert!(
        text_content(&response).contains("Applied: Bind Mod+F to window-close"),
        "{response}"
    );
    assert!(
        confirmation_code(&response).is_none(),
        "a dialog needs no code"
    );
    assert!(current_config(&home).contains("\"Mod+F\" = \"window-close\""));
    assert_eq!(server.approval_questions.len(), 1);
    assert!(
        server.approval_questions[0].contains("+ \"Mod+F\" = \"window-close\""),
        "{:?}",
        server.approval_questions
    );
}

#[test]
fn declining_or_unticking_in_the_dialog_writes_nothing() {
    for reply in [
        json!({"action": "decline"}),
        json!({"action": "cancel"}),
        json!({"action": "accept", "content": {"apply": false}}),
    ] {
        let home = home_with_config(KEYBIND_CONFIG);
        let (_fake, mut server) = start_editor(&home, "full", two_outputs(), Some(reply.clone()));

        let response = server.call_tool("set_option", json!({"key":"layout.gap","value":20}));

        assert!(
            text_content(&response).contains("did not approve"),
            "{reply}: {response}"
        );
        assert_eq!(current_config(&home), KEYBIND_CONFIG, "{reply}");
        assert_eq!(files_beside_config(&home), vec!["config.toml"], "{reply}");
    }
}

#[test]
fn set_option_checks_keys_and_values_before_asking() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let error = |server: &mut McpProcess, key: &str, value: Value| {
        error_message(&server.call_tool("set_option", json!({"key": key, "value": value})))
    };

    assert!(error(&mut server, "layout.gap", json!(9000)).contains("between 0 and 500"));
    assert!(error(&mut server, "layout.gaps", json!(1)).contains("not an Umbriel setting"));
    assert!(error(&mut server, "keybinds.Mod+T", json!("window-close")).contains("set_keybind"));
    assert!(error(&mut server, "window_rule", json!({})).contains("rule list"));
    assert_eq!(current_config(&home), KEYBIND_CONFIG);
}

#[test]
fn reset_option_removes_a_setting_after_approval() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let applied = call_and_approve(&mut server, "reset_option", json!({"key":"layout.gap"}));

    assert!(
        text_content(&applied).contains("Reset layout.gap to Umbriel's default (8) (was 4)"),
        "{applied}"
    );
    assert!(!current_config(&home).contains("gap ="));
}

#[test]
fn no_exec_refuses_settings_that_run_programs_or_spawn_actions() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "no-exec", two_outputs(), None);

    let autostart = server.call_tool(
        "set_option",
        json!({"key":"general.autostart","value":["sh"]}),
    );
    let corner_spawn = server.call_tool(
        "set_option",
        json!({"key":"hot_corners.top_left.action","value":"spawn:foot"}),
    );
    let corner_overview = server.call_tool(
        "set_option",
        json!({"key":"hot_corners.top_left.action","value":"overview-toggle"}),
    );
    let reset_autostart = server.call_tool("reset_option", json!({"key":"general.autostart"}));

    assert_refused_by_profile(&autostart, "no-exec");
    assert_refused_by_profile(&corner_spawn, "no-exec");
    assert!(
        confirmation_code(&corner_overview).is_some(),
        "{corner_overview}"
    );
    assert!(
        confirmation_code(&reset_autostart).is_some(),
        "removing autostart grants nothing, so no-exec may preview it: {reset_autostart}"
    );
}

#[test]
fn read_only_refuses_every_config_write_tool() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "read-only", two_outputs(), None);

    for (tool, arguments) in [
        ("set_option", json!({"key":"layout.gap","value":12})),
        ("reset_option", json!({"key":"layout.gap"})),
        (
            "apply_config_change",
            json!({"confirmation":"123456789abc"}),
        ),
    ] {
        assert_refused_by_profile(&server.call_tool(tool, arguments), "read-only");
    }
}

#[test]
fn disabling_the_only_enabled_output_is_refused() {
    let home = home_with_config(KEYBIND_CONFIG);
    let only_dp =
        json!([{"name": "DP-1", "enabled": true}, {"name": "HDMI-A-1", "enabled": false}]);
    let (_fake, mut server) = start_editor(&home, "full", only_dp, None);

    let last = server.call_tool(
        "set_option",
        json!({"key":"output.DP-1.enabled","value":false}),
    );
    let other = server.call_tool(
        "set_option",
        json!({"key":"output.HDMI-A-1.enabled","value":false}),
    );

    assert!(error_message(&last).contains("without a screen"), "{last}");
    assert!(confirmation_code(&other).is_some(), "{other}");
}

#[test]
fn a_keyboard_layout_change_carries_a_caution() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let previewed = server.call_tool(
        "set_option",
        json!({"key":"input.keyboard.layout","value":"de"}),
    );

    assert!(
        text_content(&previewed).contains("Caution: a wrong keyboard setting"),
        "{previewed}"
    );
}

#[test]
fn every_config_write_tool_is_marked_destructive() {
    let fake = FakeUmbriel::start(canned_replies());
    let mut server = McpProcess::start(&fake.path);

    let tools = server.request("tools/list", json!({}));
    for name in [
        "set_option",
        "reset_option",
        "set_keybind",
        "remove_keybind",
        "apply_config_change",
    ] {
        let tool = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(tool["annotations"]["destructiveHint"], true, "{name}");
    }
}

fn backups(home: &std::path::Path) -> usize {
    files_beside_config(home)
        .iter()
        .filter(|name| name.starts_with("config.toml.bak-"))
        .count()
}

#[test]
fn the_dialog_asks_whether_to_keep_a_backup_and_defaults_to_yes() {
    let home = home_with_config(KEYBIND_CONFIG);
    let approve = json!({"action": "accept", "content": {"apply": true}});
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), Some(approve));

    let response = server.call_tool("set_option", json!({"key":"layout.gap","value":12}));

    let backup_field = &server.approval_forms[0]["properties"]["backup"];
    assert_eq!(
        backup_field["type"], "boolean",
        "{:?}",
        server.approval_forms
    );
    assert_eq!(backup_field["default"], true, "{:?}", server.approval_forms);
    assert_eq!(
        backups(&home),
        1,
        "an unanswered backup question keeps the backup"
    );
    assert!(
        text_content(&response).contains("Backup of the previous config"),
        "{response}"
    );
}

#[test]
fn unticking_backup_in_the_dialog_writes_the_change_without_a_backup() {
    let home = home_with_config(KEYBIND_CONFIG);
    let approve = json!({"action": "accept", "content": {"apply": true, "backup": false}});
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), Some(approve));

    let response = server.call_tool("set_option", json!({"key":"layout.gap","value":12}));

    assert!(current_config(&home).contains("gap = 12"));
    assert_eq!(backups(&home), 0, "{:?}", files_beside_config(&home));
    assert!(
        text_content(&response).contains("No backup was kept"),
        "{response}"
    );
}

#[test]
fn the_code_path_asks_about_the_backup_too() {
    let home = home_with_config(KEYBIND_CONFIG);
    let (_fake, mut server) = start_editor(&home, "full", two_outputs(), None);

    let first = server.call_tool("set_option", json!({"key":"layout.gap","value":12}));
    assert!(
        text_content(&first).contains("whether they want a backup"),
        "{first}"
    );
    let without = server.call_tool(
        "apply_config_change",
        json!({"confirmation": confirmation_code(&first).unwrap(), "backup": false}),
    );
    assert!(
        text_content(&without).contains("No backup was kept"),
        "{without}"
    );
    assert_eq!(backups(&home), 0);

    let second = server.call_tool("set_option", json!({"key":"layout.gap","value":14}));
    server.call_tool(
        "apply_config_change",
        json!({"confirmation": confirmation_code(&second).unwrap()}),
    );
    assert_eq!(backups(&home), 1, "omitting the answer keeps a backup");
}
