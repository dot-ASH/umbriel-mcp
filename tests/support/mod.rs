#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);
const RUN_FOLDER_PREFIX: &str = "umbriel-tests-";

fn make_writable_throughout(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
        make_writable_throughout(&entry.path());
    }
}

fn remove_runs_of_finished_processes() {
    for entry in std::fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix(RUN_FOLDER_PREFIX) else {
            continue;
        };
        let finished = !Path::new("/proc").join(pid).exists();
        if finished && pid.parse::<u32>().is_ok() {
            make_writable_throughout(&entry.path());
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn run_folder() -> &'static Path {
    static RUN: OnceLock<PathBuf> = OnceLock::new();
    RUN.get_or_init(|| {
        remove_runs_of_finished_processes();
        let folder =
            std::env::temp_dir().join(format!("{RUN_FOLDER_PREFIX}{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("create the test run folder");
        folder
    })
}

pub fn scratch_directory(purpose: &str) -> PathBuf {
    let directory = run_folder().join(format!(
        "{purpose}-{}",
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&directory).expect("create scratch directory");
    directory
}

pub fn error_message(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error response, got {response}"))
        .to_owned()
}

pub struct FakeUmbriel {
    pub path: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    state: Arc<Mutex<FakeState>>,
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeUmbriel {
    pub fn start(replies: HashMap<String, Value>) -> Self {
        Self::start_with_focus(replies, Focus::Immediate)
    }

    pub fn start_with_focus(replies: HashMap<String, Value>, focus: Focus) -> Self {
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
        let state = Arc::new(Mutex::new(FakeState {
            replies,
            focus,
            pending_focus: None,
        }));
        let worker_state = Arc::clone(&state);
        let worker = thread::spawn(move || {
            while worker_running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => handle_fake_request(
                        stream,
                        &mut worker_state.lock().expect("fake state lock"),
                        &worker_requests,
                    ),
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
            state,
            running,
            worker: Some(worker),
        }
    }

    pub fn set_reply(&self, command: &str, reply: Value) {
        self.state
            .lock()
            .expect("fake state lock")
            .replies
            .insert(command.to_owned(), reply);
    }

    pub fn requests(&self) -> Vec<Value> {
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

#[derive(Clone, Copy)]
pub enum Focus {
    Immediate,
    After(Duration),
    Never,
}

struct FakeState {
    replies: HashMap<String, Value>,
    focus: Focus,
    pending_focus: Option<(String, Instant)>,
}

impl FakeState {
    fn request_focus(&mut self, window_id: &str) {
        let ready_at = match self.focus {
            Focus::Immediate => Instant::now(),
            Focus::After(delay) => Instant::now() + delay,
            Focus::Never => return,
        };
        self.pending_focus = Some((window_id.to_owned(), ready_at));
    }

    fn apply_due_focus(&mut self) {
        if let Some((window_id, ready_at)) = self.pending_focus.clone()
            && Instant::now() >= ready_at
        {
            self.replies.insert(
                "focused-window".to_owned(),
                json!({"ok": {"id": window_id, "active": true, "focused": true}}),
            );
            self.pending_focus = None;
        }
    }
}

fn handle_fake_request(
    stream: UnixStream,
    state: &mut FakeState,
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
    let action = request
        .get("arg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if command == "msg"
        && let Some(window_id) = action
            .strip_prefix("window-focus-warp:")
            .or_else(|| action.strip_prefix("window-focus:"))
    {
        state.request_focus(window_id);
    }
    state.apply_due_focus();
    let response = state
        .replies
        .get(command)
        .cloned()
        .unwrap_or_else(|| json!({"ok": []}));
    let mut stream = stream.into_inner();
    let _ = writeln!(stream, "{response}");
}

pub struct McpProcess {
    pub child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    approval_reply: Option<Value>,
    pub approval_questions: Vec<String>,
    pub approval_forms: Vec<Value>,
}

impl McpProcess {
    pub fn start(socket_path: &std::path::Path) -> Self {
        Self::start_with_args(socket_path, &[])
    }

    pub fn start_with_profile(socket_path: &std::path::Path, profile: &str) -> Self {
        Self::start_with_args(socket_path, &["--profile", profile])
    }

    pub fn start_with_args(socket_path: &std::path::Path, args: &[&str]) -> Self {
        Self::start_with_environment(socket_path, args, &[])
    }

    pub fn start_with_environment(
        socket_path: &std::path::Path,
        args: &[&str],
        environment: &[(&str, &std::ffi::OsStr)],
    ) -> Self {
        Self::start_as_client(socket_path, args, environment, json!({}), None)
    }

    pub fn start_as_client(
        socket_path: &std::path::Path,
        args: &[&str],
        environment: &[(&str, &std::ffi::OsStr)],
        capabilities: Value,
        approval_reply: Option<Value>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"));
        command
            .args(args)
            .envs(environment.iter().copied())
            .env("UMBRIEL_SOCKET", socket_path);
        Self::spawn(command, capabilities, approval_reply)
    }

    pub fn start_live() -> Self {
        Self::spawn(
            Command::new(env!("CARGO_BIN_EXE_umbriel-mcp")),
            json!({}),
            None,
        )
    }

    fn spawn(mut command: Command, capabilities: Value, approval_reply: Option<Value>) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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

    pub fn notify(&mut self, method: &str, params: Value) {
        let message = json!({"jsonrpc":"2.0", "method":method, "params":params});
        writeln!(self.input, "{message}").expect("write MCP notification");
        self.input.flush().expect("flush MCP notification");
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
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

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name, "arguments":arguments}))
    }

    pub fn call_text(&mut self, tool: &str, arguments: Value) -> String {
        let response = self.call_tool(tool, arguments);
        assert!(response.get("error").is_none(), "{tool} failed: {response}");
        text_content(&response)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.input.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn text_content(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .expect("tool result content")
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
