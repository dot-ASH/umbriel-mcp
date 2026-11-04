mod support;

use serde_json::{Value, json};
use std::io::Read;
use std::time::{Duration, Instant};
use support::McpProcess;
use wl_clipboard_rs::copy::{self, MimeType as CopyMime, Options, Source};
use wl_clipboard_rs::paste::{self, ClipboardType, MimeType as PasteMime, Seat};

fn write_clipboard(mcp: &mut McpProcess, text: &str) {
    let response = mcp.call_tool("clipboard_write", json!({"text":text}));
    assert!(
        response.get("result").is_some(),
        "clipboard_write returned an error response"
    );
}

fn read_clipboard(mcp: &mut McpProcess) -> Value {
    mcp.call_tool("clipboard_read", json!({}))
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
    let mut mcp = McpProcess::start_live();
    for round in 0..20 {
        let text = format!("round {round}: línea uno\nline two \u{2713} \u{1F600}");
        write_clipboard(&mut mcp, &text);
        assert_eq!(clipboard_text_seen_by_other_clients(), text);
    }
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn written_text_round_trips_through_read_without_delay() {
    let _restore = RestoreClipboard::capture();
    let mut mcp = McpProcess::start_live();
    for round in 0..20 {
        let text = format!("round trip {round}");
        write_clipboard(&mut mcp, &text);
        assert_eq!(tool_text(&read_clipboard(&mut mcp)), text);
    }
}

#[test]
#[ignore = "requires a running Umbriel session with data-control clipboard support"]
fn write_returns_promptly() {
    let _restore = RestoreClipboard::capture();
    let mut mcp = McpProcess::start_live();
    let started = Instant::now();
    write_clipboard(&mut mcp, "promptness check");
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
    let mut mcp = McpProcess::start_live();
    assert_eq!(tool_text(&read_clipboard(&mut mcp)), "set by someone else");
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

    let mut mcp = McpProcess::start_live();
    let response = read_clipboard(&mut mcp);
    let message = response["error"]["message"]
        .as_str()
        .expect("reading a clipboard without text should be an error")
        .to_lowercase();
    assert!(
        message.contains("no text") || message.contains("not text"),
        "error should say the clipboard holds no text, got: {message}"
    );
}
