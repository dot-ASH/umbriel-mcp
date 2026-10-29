use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

fn socket_path() -> Result<String> {
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

pub fn dispatch(action: &str) -> Result<Value> {
    request(json!({ "cmd": "msg", "arg": action }))
}

pub fn windows() -> Result<Vec<Value>> {
    Ok(query("windows")?.as_array().cloned().unwrap_or_default())
}

pub fn window_geometry(window_id: &str) -> Result<String> {
    let matching = windows()?
        .into_iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(window_id))
        .ok_or_else(|| anyhow!("no window with id {window_id}"))?;

    let number = |key: &str| -> Result<i64> {
        matching
            .get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("window {window_id} has no {key}"))
    };
    Ok(format!(
        "{},{} {}x{}",
        number("x")?,
        number("y")?,
        number("w")?,
        number("h")?
    ))
}
