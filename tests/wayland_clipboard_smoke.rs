use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use wl_clipboard_rs::copy::{self, MimeType as CopyMime, Options, Source};
use wl_clipboard_rs::paste::{self, ClipboardType, MimeType as PasteMime, Seat};

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
        process.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "clipboard-smoke", "version": "1" }
            }),
        );
        writeln!(
            process.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
        )
        .expect("write initialized notification");
        process
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
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
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }

    fn write_clipboard(&mut self, text: &str) {
        let response = self.call_tool("clipboard_write", json!({"text":text}));
        assert!(
            response.get("result").is_some(),
            "clipboard_write returned an error response"
        );
    }

    fn read_clipboard(&mut self) -> Value {
        self.call_tool("clipboard_read", json!({}))
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_text(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response should carry text content")
        .to_owned()
}

struct RestoreClipboard {
    original_text: Option<String>,
}

impl RestoreClipboard {
    fn capture() -> Self {
        let original_text =
            paste::get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMime::Text)
                .ok()
                .and_then(|(mut reader, _)| {
                    let mut contents = String::new();
                    reader.read_to_string(&mut contents).ok()?;
                    Some(contents)
                });
        Self { original_text }
    }
}

impl Drop for RestoreClipboard {
    fn drop(&mut self) {
        match &self.original_text {
            Some(text) => set_clipboard_as_other_client(text),
            None => {
                let _ = copy::clear(copy::ClipboardType::Regular, copy::Seat::All);
            }
        }
    }
}

fn clipboard_text_seen_by_other_clients() -> String {
    let (mut reader, _) =
        paste::get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMime::Text)
            .expect("independent client could not read the clipboard");
    let mut contents = String::new();
    reader
        .read_to_string(&mut contents)
        .expect("read clipboard contents");
    contents
}

fn set_clipboard_as_other_client(text: &str) {
    let mut options = Options::new();
    options.foreground(false);
    options
        .copy(Source::Bytes(text.as_bytes().into()), CopyMime::Text)
        .expect("independent client could not set the clipboard");
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn written_text_is_immediately_visible_to_other_clients() {
    let _restore = RestoreClipboard::capture();
    let mut mcp = McpProcess::start();
    for round in 0..20 {
        let text = format!("round {round}: línea uno\nline two \u{2713} \u{1F600}");
        mcp.write_clipboard(&text);
        assert_eq!(clipboard_text_seen_by_other_clients(), text);
    }
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn written_text_round_trips_through_read_without_delay() {
    let _restore = RestoreClipboard::capture();
    let mut mcp = McpProcess::start();
    for round in 0..20 {
        let text = format!("round trip {round}");
        mcp.write_clipboard(&text);
        assert_eq!(tool_text(&mcp.read_clipboard()), text);
    }
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn write_returns_promptly() {
    let _restore = RestoreClipboard::capture();
    let mut mcp = McpProcess::start();
    let started = Instant::now();
    mcp.write_clipboard("promptness check");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "clipboard_write took {:?}",
        started.elapsed()
    );
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn read_returns_text_set_by_another_client() {
    let _restore = RestoreClipboard::capture();
    set_clipboard_as_other_client("set by someone else");
    let mut mcp = McpProcess::start();
    assert_eq!(tool_text(&mcp.read_clipboard()), "set by someone else");
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn read_reports_a_clear_error_when_the_clipboard_holds_no_text() {
    let _restore = RestoreClipboard::capture();
    let mut options = Options::new();
    options.foreground(false);
    options
        .copy(
            Source::Bytes(vec![0x89, b'P', b'N', b'G'].into()),
            CopyMime::Specific("image/png".to_owned()),
        )
        .expect("independent client could not set a non-text clipboard");

    let mut mcp = McpProcess::start();
    let response = mcp.read_clipboard();
    let message = response["error"]["message"]
        .as_str()
        .expect("reading a clipboard without text should be an error")
        .to_lowercase();
    assert!(
        message.contains("no text") || message.contains("not text"),
        "error should say the clipboard holds no text, got: {message}"
    );
}
