mod support;

use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::McpProcess;

fn wait_for_window(mcp: &mut McpProcess, title: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let windows: Value =
            serde_json::from_str(&mcp.call_text("list_windows", json!({}))).expect("window list");
        if let Some(window) = windows
            .as_array()
            .into_iter()
            .flatten()
            .find(|window| window["title"].as_str() == Some(title))
        {
            return window.clone();
        }
        assert!(Instant::now() < deadline, "test terminal did not appear");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_until_active(mcp: &mut McpProcess, window_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !mcp
        .call_text("get_active_window", json!({}))
        .contains(window_id)
    {
        assert!(
            Instant::now() < deadline,
            "window never became active; is the session locked?"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn first_screen_position(report: &str) -> (i64, i64) {
    let after = report
        .split("at screen (")
        .nth(1)
        .unwrap_or_else(|| panic!("no match reported: {report}"));
    let coordinates = after.split(')').next().expect("coordinates");
    let (x, y) = coordinates.split_once(", ").expect("x, y");
    (x.parse().expect("x"), y.parse().expect("y"))
}

#[test]
#[ignore = "requires an unlocked Umbriel session with kitty, grim and tesseract"]
fn ocr_reads_a_real_window_and_locates_its_text_on_screen() {
    let title = "umbriel-mcp-ocr-smoke";
    let command = format!(
        "kitty --title={title} -o font_size=22 sh -c \"printf 'Umbriel OCR Probe\\n\\nSettings   Save   Cancel\\n\\nquick brown fox 12345\\n'; sleep 120\""
    );
    let mut mcp = McpProcess::start_live();
    mcp.call_text("launch_app", json!({"command":command}));
    let window = wait_for_window(&mut mcp, title);
    let window_id = window["id"].as_str().expect("window id").to_owned();
    mcp.call_text("focus_window", json!({"window_id":window_id}));
    wait_until_active(&mut mcp, &window_id);
    std::thread::sleep(Duration::from_millis(500));

    let read = mcp.call_text("screenshot_with_ocr", json!({"window_id":window_id}));
    let located = mcp.call_text("find_text_on_screen", json!({"target":"Cancel"}));
    mcp.call_text("close_window", json!({"window_id":window_id}));

    for expected in [
        "Umbriel OCR Probe",
        "Settings",
        "Save",
        "Cancel",
        "quick brown fox",
    ] {
        assert!(read.contains(expected), "missing {expected:?} in:\n{read}");
    }
    let (x, y) = first_screen_position(&located);
    let (left, top) = (
        window["x"].as_i64().expect("x"),
        window["y"].as_i64().expect("y"),
    );
    let (width, height) = (
        window["w"].as_i64().expect("w"),
        window["h"].as_i64().expect("h"),
    );
    assert!(
        (left..left + width).contains(&x) && (top..top + height).contains(&y),
        "Cancel located at ({x}, {y}), outside the window {left},{top} {width}x{height}"
    );
}
