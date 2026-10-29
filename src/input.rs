use anyhow::{anyhow, Result};
use std::io::Write;
use std::process::{Command, Stdio};

const PRESS_AND_RELEASE: u8 = 0xC0;
const PRESS_ONLY: u8 = 0x40;
const RELEASE_ONLY: u8 = 0x80;

fn run(program: &str, args: &[String]) -> Result<String> {
    let result = Command::new(program)
        .args(args)
        .output()
        .map_err(|cause| anyhow!("cannot run {program}: {cause}"))?;
    if !result.status.success() {
        return Err(anyhow!(
            "{program} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

fn button_base(name: &str) -> Result<u8> {
    match name {
        "left" => Ok(0x00),
        "right" => Ok(0x01),
        "middle" => Ok(0x02),
        "side" => Ok(0x03),
        other => Err(anyhow!(
            "unknown button {other}; expected left, right, middle or side"
        )),
    }
}

fn ydotool(args: &[String]) -> Result<()> {
    run("ydotool", args).map(|_| ())
}

pub fn move_pointer(x: i32, y: i32) -> Result<()> {
    ydotool(&[
        "mousemove".into(),
        "--absolute".into(),
        "-x".into(),
        x.to_string(),
        "-y".into(),
        y.to_string(),
    ])
}

pub fn click(button: &str, double: bool) -> Result<()> {
    let code = format!("0x{:02X}", button_base(button)? | PRESS_AND_RELEASE);
    let mut args = vec!["click".to_string()];
    if double {
        args.push("--repeat".into());
        args.push("2".into());
    }
    args.push(code);
    ydotool(&args)
}

pub fn scroll(direction: &str, amount: i32) -> Result<()> {
    let steps = match direction {
        "down" => amount,
        "up" => -amount,
        other => return Err(anyhow!("unknown direction {other}; expected up or down")),
    };
    ydotool(&[
        "mousemove".into(),
        "--wheel".into(),
        "-x".into(),
        "0".into(),
        "-y".into(),
        steps.to_string(),
    ])
}

pub fn drag(from: (i32, i32), to: (i32, i32), button: &str) -> Result<()> {
    let base = button_base(button)?;
    move_pointer(from.0, from.1)?;
    ydotool(&["click".into(), format!("0x{:02X}", base | PRESS_ONLY)])?;
    move_pointer(to.0, to.1)?;
    ydotool(&["click".into(), format!("0x{:02X}", base | RELEASE_ONLY)])
}

pub fn type_text(text: &str, delay_ms: u32) -> Result<()> {
    let mut args = Vec::new();
    if delay_ms > 0 {
        args.push("-d".to_string());
        args.push(delay_ms.to_string());
    }
    args.push("--".to_string());
    args.push(text.to_string());
    run("wtype", &args).map(|_| ())
}

pub fn press_keys(chord: &str) -> Result<()> {
    let mut parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .collect();
    let key = parts
        .pop()
        .ok_or_else(|| anyhow!("empty key chord"))?
        .to_string();
    let modifiers: Vec<String> = parts.iter().map(|m| m.to_lowercase()).collect();

    let mut args = Vec::new();
    for modifier in &modifiers {
        args.push("-M".to_string());
        args.push(modifier.clone());
    }
    args.push("-k".to_string());
    args.push(key);
    for modifier in modifiers.iter().rev() {
        args.push("-m".to_string());
        args.push(modifier.clone());
    }
    run("wtype", &args).map(|_| ())
}

pub fn clipboard_read() -> Result<String> {
    run("wl-paste", &["--no-newline".to_string()])
}

pub fn clipboard_write(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|cause| anyhow!("cannot run wl-copy: {cause}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("wl-copy stdin unavailable"))?
        .write_all(text.as_bytes())?;

    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("wl-copy exited with {status}"));
    }
    Ok(())
}
