mod support;

use serde_json::{Value, json};
use support::McpProcess;

#[test]
#[ignore = "needs a running Umbriel session with at least one window"]
fn every_window_is_captured_straight_from_umbriel_at_its_own_size() {
    let mut mcp = McpProcess::start_live();
    let windows: Value =
        serde_json::from_str(&mcp.call_text("list_windows", json!({}))).expect("windows JSON");

    for window in windows.as_array().expect("windows") {
        let response = mcp.call_tool("screenshot", json!({"window_id": window["id"]}));
        let facts = &response["result"]["structuredContent"];
        assert_eq!(facts["captured_by"], "umbriel", "{response}");
        assert_eq!(facts["covers"]["width"], window["w"], "{response}");
        assert_eq!(facts["covers"]["height"], window["h"], "{response}");
    }
}
