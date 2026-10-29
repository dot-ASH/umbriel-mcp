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
}

impl McpProcess {
    fn start(socket_path: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
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
        };

        let initialized = process.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
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
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                return response;
            }
        }
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
