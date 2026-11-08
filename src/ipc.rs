use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn socket_path() -> Result<String> {
    if let Ok(explicit) = std::env::var("UMBRIEL_SOCKET") {
        return Ok(explicit);
    }
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .context("neither UMBRIEL_SOCKET nor XDG_RUNTIME_DIR is set")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    Ok(format!("{runtime_dir}/umbriel-{display}.sock"))
}

fn request(payload: Value) -> Result<Value> {
    let path = socket_path()?;
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("cannot reach umbriel at {path}; is the compositor running?"))?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT))?;

    let mut outgoing = stream.try_clone()?;
    writeln!(outgoing, "{payload}")?;
    outgoing.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value =
        serde_json::from_str(&line).with_context(|| format!("unparseable reply: {line}"))?;

    if let Some(failure) = reply.get("err") {
        let detail = failure
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| failure.to_string());
        return Err(anyhow!("umbriel rejected the request: {detail}"));
    }
    reply
        .get("ok")
        .cloned()
        .ok_or_else(|| anyhow!("reply carried neither ok nor err: {line}"))
}

pub fn query(command: &str) -> Result<Value> {
    request(json!({ "cmd": command }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    TypedActions,
    NameColonArgument,
}

static DETECTED_PROTOCOL: std::sync::Mutex<Option<Protocol>> = std::sync::Mutex::new(None);

fn is_unknown_command(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("unknown command")
}

pub fn detect_protocol() -> Result<Protocol> {
    match request(json!({ "cmd": "action" })) {
        Err(error) if is_unknown_command(&error) => Ok(Protocol::NameColonArgument),
        Err(error) if format!("{error:#}").contains("umbriel rejected") => {
            Ok(Protocol::TypedActions)
        }
        Err(error) => Err(error),
        Ok(_) => Ok(Protocol::TypedActions),
    }
}

fn protocol() -> Result<Protocol> {
    let mut detected = DETECTED_PROTOCOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(protocol) = *detected {
        return Ok(protocol);
    }
    let protocol = detect_protocol()?;
    *detected = Some(protocol);
    Ok(protocol)
}

fn forget_protocol() {
    *DETECTED_PROTOCOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn umbriel_version() -> String {
    std::process::Command::new("umbriel")
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "version unknown".to_owned())
}

fn protocol_mismatch(error: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{error:#}. This Umbriel ({}) does not accept the action request umbriel-mcp {} sent, \
         so its IPC protocol has changed. Run `umbriel-mcp update`; if the newest umbriel-mcp \
         still fails, report it at https://github.com/dot-ASH/umbriel-mcp/issues.",
        umbriel_version(),
        env!("CARGO_PKG_VERSION")
    )
}

fn with_protocol(send: impl Fn(Protocol) -> Result<()>) -> Result<()> {
    match send(protocol()?) {
        Err(error) if is_unknown_command(&error) => {
            forget_protocol();
            send(protocol()?).map_err(|error| {
                if is_unknown_command(&error) {
                    protocol_mismatch(error)
                } else {
                    error
                }
            })
        }
        other => other,
    }
}

pub fn perform(action: &crate::actions::CompositorAction) -> Result<()> {
    with_protocol(|protocol| {
        match protocol {
            Protocol::TypedActions => request(action.typed_request()),
            Protocol::NameColonArgument => {
                request(json!({ "cmd": "msg", "arg": action.legacy_text() }))
            }
        }
        .map(|_| ())
    })
}

fn run_through_umbriel_command(words: &[String]) -> Result<()> {
    let output = std::process::Command::new("umbriel")
        .arg("msg")
        .args(words)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|cause| {
            anyhow!("cannot run the umbriel command ({cause}); is Umbriel installed and on PATH?")
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&[output.stdout, output.stderr].concat())
        .trim()
        .to_owned();
    Err(anyhow!("umbriel rejected the request: {detail}"))
}

pub fn perform_text(action: &str) -> Result<()> {
    with_protocol(|protocol| match protocol {
        Protocol::TypedActions => {
            run_through_umbriel_command(&crate::actions::words_for_command_line(action)?)
        }
        Protocol::NameColonArgument => request(json!({
            "cmd": "msg",
            "arg": crate::actions::legacy_text_for(action)?
        }))
        .map(|_| ()),
    })
}

pub fn focused_window() -> Result<Value> {
    match query("focused-window") {
        Err(error) if format!("{error:#}").contains("no focused window") => Ok(Value::Null),
        other => other,
    }
}

pub fn windows() -> Result<Vec<Value>> {
    Ok(query("windows")?.as_array().cloned().unwrap_or_default())
}

pub struct WindowPlacement {
    pub geometry: String,
    pub origin: (i64, i64),
    pub size: (i64, i64),
    pub shown_on_screen: bool,
    pub on_screen: crate::placement::OnScreen,
    pub output_area: Option<crate::placement::Rect>,
    pub drawn_over: Vec<crate::placement::Covering>,
    pub title: Option<String>,
}

pub fn listed(command: &str) -> Vec<Value> {
    query(command)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

pub fn window_placement(window_id: &str) -> Result<WindowPlacement> {
    let all_windows = windows()?;
    let matching = all_windows
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(window_id))
        .cloned()
        .ok_or_else(|| anyhow!("no window with id {window_id}"))?;
    let outputs = listed("outputs");
    let on_screen = crate::placement::on_screen(&matching, &outputs);
    let output_area = matching
        .get("output")
        .and_then(Value::as_str)
        .and_then(|name| crate::placement::output_rect(&outputs, name));
    let drawn_over =
        crate::placement::things_drawn_over(&matching, &all_windows, &listed("layers"));

    let number = |key: &str| -> Result<i64> {
        matching
            .get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("window {window_id} has no {key}"))
    };
    let (x, y) = (number("x")?, number("y")?);
    let (width, height) = (number("w")?, number("h")?);
    Ok(WindowPlacement {
        geometry: format!("{x},{y} {width}x{height}"),
        origin: (x, y),
        size: (width, height),
        shown_on_screen: on_screen.any_part_shown(),
        on_screen,
        output_area,
        drawn_over,
        title: matching
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
