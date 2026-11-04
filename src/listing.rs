use serde_json::{Map, Value};

const BRIEF_WINDOW_FIELDS: &[&str] = &[
    "id",
    "app_id",
    "title",
    "output",
    "workspace",
    "visible",
    "floating",
    "fullscreen",
    "minimized",
    "x",
    "y",
    "w",
    "h",
];

const BRIEF_OUTPUT_FIELDS: &[&str] = &[
    "name",
    "description",
    "enabled",
    "focused",
    "position",
    "logical_size",
    "scale",
    "transform",
    "active_workspace",
];

fn keep_fields(entry: &Value, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| Some(((*field).to_owned(), entry.get(*field)?.clone())))
        .collect()
}

pub fn brief_window(window: &Value, keyboard_focus_id: Option<&str>) -> Value {
    if !window.is_object() {
        return window.clone();
    }
    let mut brief = keep_fields(window, BRIEF_WINDOW_FIELDS);
    let has_keyboard_focus = keyboard_focus_id.is_some()
        && window.get("id").and_then(Value::as_str) == keyboard_focus_id;
    brief.insert(
        "has_keyboard_focus".to_owned(),
        Value::Bool(has_keyboard_focus),
    );
    Value::Object(brief)
}

fn current_mode(output: &Value) -> Option<String> {
    let mode = output
        .get("modes")?
        .as_array()?
        .iter()
        .find(|mode| mode.get("current").and_then(Value::as_bool) == Some(true))?;
    let width = mode.get("width")?.as_u64()?;
    let height = mode.get("height")?.as_u64()?;
    let hertz = mode.get("refresh_mhz")?.as_u64()? as f64 / 1000.0;
    Some(format!(
        "{width}x{height}@{}Hz",
        (hertz * 100.0).round() / 100.0
    ))
}

pub fn brief_output(output: &Value) -> Value {
    if !output.is_object() {
        return output.clone();
    }
    let mut brief = keep_fields(output, BRIEF_OUTPUT_FIELDS);
    if let Some(mode) = current_mode(output) {
        brief.insert("mode".to_owned(), Value::String(mode));
    }
    Value::Object(brief)
}

pub fn shown(entries: Value, detail: bool, brief: impl Fn(&Value) -> Value) -> Value {
    if detail {
        return entries;
    }
    match entries {
        Value::Array(items) => Value::Array(items.iter().map(&brief).collect()),
        single => brief(&single),
    }
}

#[cfg(test)]
mod tests {
    use super::{brief_output, brief_window};
    use serde_json::json;

    #[test]
    fn a_brief_window_keeps_only_what_identifies_and_places_it() {
        let window = json!({"id": "a", "title": "t", "x": 1, "pid": 9, "focused": true});
        assert_eq!(
            brief_window(&window, Some("a")),
            json!({"id": "a", "title": "t", "x": 1, "has_keyboard_focus": true})
        );
        assert_eq!(
            brief_window(&window, Some("b"))["has_keyboard_focus"],
            false
        );
        assert_eq!(brief_window(&window, None)["has_keyboard_focus"], false);
    }

    #[test]
    fn a_brief_output_names_its_current_mode() {
        let brief = brief_output(&json!({
            "name": "DP-1",
            "modes": [
                {"width": 2560, "height": 1440, "refresh_mhz": 143856, "current": true},
                {"width": 1920, "height": 1080, "refresh_mhz": 60000, "current": false}
            ]
        }));
        assert_eq!(brief, json!({"name": "DP-1", "mode": "2560x1440@143.86Hz"}));
    }

    #[test]
    fn no_focused_window_stays_as_umbriel_reported_it() {
        assert_eq!(brief_window(&json!(null), None), json!(null));
    }
}
