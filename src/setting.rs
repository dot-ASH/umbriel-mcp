use crate::config_edit::{
    PreparedChange, load_user_config, prepare, remove_keeping_comments_above,
};
use anyhow::{Context, Result, anyhow};
use serde_json::Value as Json;
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

const NAME_PLACEHOLDER: &str = "<name>";
const SECTION_TYPES: [&str; 2] = ["table", "map"];
const KEYBOARD_KEYS: [&str; 4] = [
    "input.keyboard.layout",
    "input.keyboard.variant",
    "input.keyboard.model",
    "input.keyboard.options",
];
const KEYS_THAT_RUN_PROGRAMS_OR_LOAD_FILES: [&str; 1] = ["general.autostart"];
const SECTIONS_THAT_RUN_PROGRAMS_OR_LOAD_FILES: [&str; 4] =
    ["events", "environment", "include", "drm"];

#[derive(Debug, PartialEq, Eq)]
pub enum Policy {
    Ordinary,
    RunsProgramsOrLoadsFiles,
    WeakensSecurity,
    TakesAnAction,
    UseKeybindTools,
    MayLockYouOut(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub struct PlannedSetting {
    pub new_text: String,
    pub previous: Option<String>,
}

fn path_of(entry: &Json) -> &str {
    entry.get("path").and_then(Json::as_str).unwrap_or_default()
}

fn type_of(entry: &Json) -> &str {
    entry.get("type").and_then(Json::as_str).unwrap_or_default()
}

fn segments_match(path: &str, key: &str) -> Option<bool> {
    let path_segments: Vec<&str> = path.split('.').collect();
    let key_segments: Vec<&str> = key.split('.').collect();
    if path_segments.len() != key_segments.len() {
        return None;
    }
    let mut exact = true;
    for (pattern, segment) in path_segments.iter().zip(&key_segments) {
        if pattern == segment {
            continue;
        }
        if *pattern == NAME_PLACEHOLDER && !segment.is_empty() {
            exact = false;
            continue;
        }
        return None;
    }
    Some(exact)
}

fn rule_list_error(key: &str) -> anyhow::Error {
    anyhow!(
        "`{key}` belongs to a rule list (such as window_rule); editing rule lists is not supported \
         yet. Read it with read_config and change it by hand."
    )
}

pub fn schema_entry<'a>(options: &'a [Json], key: &str) -> Result<&'a Json> {
    let key = key.trim();
    if key.contains('[') {
        return Err(rule_list_error(key));
    }
    let mut placeholder_match = None;
    for option in options {
        match segments_match(path_of(option), key) {
            Some(true) => {
                placeholder_match = Some(option);
                break;
            }
            Some(false) if placeholder_match.is_none() => placeholder_match = Some(option),
            _ => {}
        }
    }
    let entry = placeholder_match.ok_or_else(|| {
        anyhow!("`{key}` is not an Umbriel setting; use config_options to find the exact key")
    })?;
    let kind = type_of(entry);
    if path_of(entry).contains("[]") || kind == "array_of_tables" {
        return Err(rule_list_error(key));
    }
    if SECTION_TYPES.contains(&kind) {
        return Err(anyhow!(
            "`{key}` is a section, not a single setting; set one of its keys instead \
             (config_options lists them)"
        ));
    }
    Ok(entry)
}

fn number_text(value: Option<&Json>) -> String {
    value.map(Json::to_string).unwrap_or_default()
}

fn check_range(entry: &Json, number: f64) -> Result<()> {
    let below = entry
        .get("min")
        .and_then(Json::as_f64)
        .is_some_and(|min| number < min);
    let above = entry
        .get("max")
        .and_then(Json::as_f64)
        .is_some_and(|max| number > max);
    if below || above {
        return Err(anyhow!(
            "`{}` must be between {} and {}",
            path_of(entry),
            number_text(entry.get("min")),
            number_text(entry.get("max"))
        ));
    }
    Ok(())
}

fn as_bool(entry: &Json, value: &Json) -> Result<Value> {
    match value {
        Json::Bool(flag) => Ok(Value::from(*flag)),
        Json::String(text) if text == "true" || text == "false" => Ok(Value::from(text == "true")),
        _ => Err(anyhow!("`{}` takes true or false", path_of(entry))),
    }
}

fn as_number(value: &Json) -> Option<f64> {
    match value {
        Json::Number(number) => number.as_f64(),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn as_int(entry: &Json, value: &Json) -> Result<Value> {
    let number =
        as_number(value).ok_or_else(|| anyhow!("`{}` takes a whole number", path_of(entry)))?;
    if number.fract() != 0.0 || !number.is_finite() {
        return Err(anyhow!("`{}` takes a whole number", path_of(entry)));
    }
    check_range(entry, number)?;
    Ok(Value::from(number as i64))
}

fn as_float(entry: &Json, value: &Json) -> Result<Value> {
    let number = as_number(value)
        .filter(|number| number.is_finite())
        .ok_or_else(|| anyhow!("`{}` takes a number", path_of(entry)))?;
    check_range(entry, number)?;
    Ok(Value::from(number))
}

fn allowed_values(entry: &Json) -> Option<Vec<&str>> {
    entry
        .get("values")
        .and_then(Json::as_array)
        .map(|values| values.iter().filter_map(Json::as_str).collect())
}

fn as_string(entry: &Json, value: &Json) -> Result<Value> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("`{}` takes a string", path_of(entry)))?;
    if let Some(allowed) = allowed_values(entry)
        && !allowed.contains(&text)
    {
        return Err(anyhow!(
            "`{}` must be one of: {}",
            path_of(entry),
            allowed.join(", ")
        ));
    }
    Ok(Value::from(text))
}

fn as_list(
    entry: &Json,
    value: &Json,
    item: impl Fn(&Json, &Json) -> Result<Value>,
    what: &str,
) -> Result<Value> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("`{}` takes a list of {what}", path_of(entry)))?;
    let mut array = Array::new();
    for element in items {
        array.push_formatted(
            item(entry, element)
                .map_err(|_| anyhow!("`{}` takes a list of {what}", path_of(entry)))?,
        );
    }
    array.fmt();
    Ok(Value::Array(array))
}

pub fn toml_value(entry: &Json, value: &Json) -> Result<Value> {
    let strings = |entry: &Json, value: &Json| as_list(entry, value, as_string, "strings");
    match type_of(entry) {
        "bool" => as_bool(entry, value),
        "int" => as_int(entry, value),
        "float" | "float_or_table" => as_float(entry, value),
        "string" | "color" | "enum" | "string_or_table" => as_string(entry, value),
        "string_array" => strings(entry, value),
        "float_array" => as_list(entry, value, as_float, "numbers"),
        "int_array" => as_list(entry, value, as_int, "whole numbers"),
        "string_or_string_array" | "enum_or_array" => {
            if value.is_array() {
                strings(entry, value)
            } else {
                as_string(entry, value)
            }
        }
        "int_or_string" => as_int(entry, value).or_else(|_| as_string(entry, value)),
        "int_or_string_array" => as_int(entry, value).or_else(|_| strings(entry, value)),
        other => Err(anyhow!(
            "`{}` has type {other}, which cannot be set as a single value",
            path_of(entry)
        )),
    }
}

pub fn policy(key: &str) -> Policy {
    let key = key.trim();
    let section = key.split('.').next().unwrap_or_default();
    if section == "keybinds" {
        return Policy::UseKeybindTools;
    }
    let loads_shader = section == "effects" && key.ends_with(".shader");
    if KEYS_THAT_RUN_PROGRAMS_OR_LOAD_FILES.contains(&key)
        || SECTIONS_THAT_RUN_PROGRAMS_OR_LOAD_FILES.contains(&section)
        || loads_shader
    {
        return Policy::RunsProgramsOrLoadsFiles;
    }
    if key == "screencast.disable_dynamic_confirmation" {
        return Policy::WeakensSecurity;
    }
    if key.ends_with(".action") {
        return Policy::TakesAnAction;
    }
    if KEYBOARD_KEYS.contains(&key) {
        return Policy::MayLockYouOut(
            "a wrong keyboard setting can stop you typing; if that happens, restore the backup from a TTY",
        );
    }
    if key == "general.mod_key" {
        return Policy::MayLockYouOut(
            "changing the modifier key changes every Mod keybind, including the ones you use to recover",
        );
    }
    Policy::Ordinary
}

fn parse(config: &str) -> Result<DocumentMut> {
    config
        .parse::<DocumentMut>()
        .context("the Umbriel config is not valid TOML, so it was not changed")
}

fn split_key(key: &str) -> Result<(Vec<&str>, &str)> {
    let mut segments: Vec<&str> = key.trim().split('.').collect();
    let leaf = segments.pop().filter(|leaf| !leaf.is_empty());
    match leaf {
        Some(leaf) if segments.iter().all(|segment| !segment.is_empty()) => Ok((segments, leaf)),
        _ => Err(anyhow!("`{key}` is not a valid setting key")),
    }
}

fn plain_text(value: &Value) -> String {
    let mut value = value.clone();
    value.decor_mut().clear();
    value.to_string()
}

fn not_a_section(prefix: &[&str]) -> anyhow::Error {
    anyhow!(
        "`{}` in your config is a value, not a section, so it was not changed",
        prefix.join(".")
    )
}

pub fn plan_set(config: &str, key: &str, mut value: Value) -> Result<PlannedSetting> {
    let (sections, leaf) = split_key(key)?;
    let mut document = parse(config)?;
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for (depth, section) in sections.iter().enumerate() {
        if table.get(section).is_none() {
            let mut created = Table::new();
            created.set_implicit(true);
            created.decor_mut().set_prefix("\n");
            table.insert(section, Item::Table(created));
        }
        table = table
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| not_a_section(&sections[..=depth]))?;
    }
    let previous = match table.get_mut(leaf) {
        Some(Item::Value(existing)) => {
            let previous = plain_text(existing);
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
            Some(previous)
        }
        Some(Item::None) | None => {
            table.insert(leaf, Item::Value(value));
            None
        }
        Some(_) => {
            return Err(anyhow!(
                "`{key}` is a section in your config, not a single setting, so it was not changed"
            ));
        }
    };
    Ok(PlannedSetting {
        new_text: document.to_string(),
        previous,
    })
}

pub fn plan_reset(config: &str, key: &str) -> Result<PlannedSetting> {
    let (sections, leaf) = split_key(key)?;
    let mut document = parse(config)?;
    let not_set = || {
        anyhow!(
            "`{key}` is not set in your config, so Umbriel already uses its default; nothing changed"
        )
    };
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for section in &sections {
        table = table
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(not_set)?;
    }
    let previous = match table.get(leaf) {
        Some(Item::Value(existing)) => plain_text(existing),
        _ => return Err(not_set()),
    };
    remove_keeping_comments_above(table, leaf);
    Ok(PlannedSetting {
        new_text: document.to_string(),
        previous: Some(previous),
    })
}

pub fn would_disable_every_output(enabled_outputs: &[String], key: &str, value: &Value) -> bool {
    let Some(output) = key
        .strip_prefix("output.")
        .and_then(|rest| rest.strip_suffix(".enabled"))
    else {
        return false;
    };
    value.as_bool() == Some(false)
        && enabled_outputs.iter().any(|name| name == output)
        && enabled_outputs.iter().all(|name| name == output)
}

fn live_enabled_outputs() -> Result<Vec<String>> {
    let outputs = crate::ipc::query("outputs")?;
    Ok(outputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| output.get("enabled").and_then(Json::as_bool) == Some(true))
        .filter_map(|output| output.get("name").and_then(Json::as_str).map(str::to_owned))
        .collect())
}

fn refuse_disabling_every_output(key: &str, value: &Value) -> Result<()> {
    let targets_output_switch = key.starts_with("output.") && key.ends_with(".enabled");
    if !targets_output_switch || value.as_bool() != Some(false) {
        return Ok(());
    }
    let enabled = live_enabled_outputs().map_err(|error| {
        anyhow!(
            "could not check which outputs are enabled ({error:#}), so `{key}` was not \
             changed: disabling an output must not leave you without a screen"
        )
    })?;
    if would_disable_every_output(&enabled, key, value) {
        return Err(anyhow!(
            "`{key} = false` would disable your only enabled output and leave you without a \
             screen, so it was not changed"
        ));
    }
    Ok(())
}

fn default_text(entry: &Json) -> String {
    entry
        .get("default")
        .map(|default| format!(" ({default})"))
        .unwrap_or_default()
}

pub fn prepare_set_option(key: &str, value: &Json) -> Result<Option<PreparedChange>> {
    let key = key.trim();
    let options = crate::compositor::schema_options()?;
    let entry = schema_entry(&options, key)?;
    let value = toml_value(entry, value)?;
    refuse_disabling_every_output(key, &value)?;
    let (config_file, original) = load_user_config()?;
    let rendered = plain_text(&value);
    let planned = plan_set(&original, key, value)?;
    let was = planned
        .previous
        .map(|previous| format!(" (was {previous})"))
        .unwrap_or_else(|| format!(" (was Umbriel's default{})", default_text(entry)));
    let caution = match policy(key) {
        Policy::MayLockYouOut(reason) => Some(reason.to_owned()),
        _ => None,
    };
    prepare(
        config_file,
        original,
        planned.new_text,
        format!("Set {key} to {rendered}{was}"),
        caution,
    )
}

pub fn prepare_reset_option(key: &str) -> Result<PreparedChange> {
    let key = key.trim();
    let options = crate::compositor::schema_options()?;
    let entry = schema_entry(&options, key)?;
    let (config_file, original) = load_user_config()?;
    let planned = plan_reset(&original, key)?;
    let summary = format!(
        "Reset {key} to Umbriel's default{} (was {})",
        default_text(entry),
        planned.previous.unwrap_or_default()
    );
    prepare(config_file, original, planned.new_text, summary, None)?
        .ok_or_else(|| anyhow!("`{key}` is already at its default; nothing changed"))
}

#[cfg(test)]
mod tests {
    use super::{
        Policy, plan_reset, plan_set, policy, schema_entry, toml_value, would_disable_every_output,
    };
    use serde_json::{Value as Json, json};
    use toml_edit::Value;

    fn options() -> Vec<Json> {
        json!([
            {"path": "layout", "type": "table"},
            {"path": "layout.gap", "type": "int", "min": 0, "max": 500, "default": 8},
            {"path": "appearance.blur.enabled", "type": "bool", "default": true},
            {"path": "appearance.blur.radius", "type": "float", "min": 0.0, "max": 64.0},
            {"path": "animation.workspaces.style", "type": "enum", "values": ["slide", "reveal"]},
            {"path": "colors.border.active", "type": "color"},
            {"path": "general.autostart", "type": "string_array", "default": []},
            {"path": "output.<name>.scale", "type": "float", "min": 0.25, "max": 8.0},
            {"path": "output.<name>.enabled", "type": "bool", "default": true},
            {"path": "keybinds.<name>", "type": "string_or_table", "format": "action"},
            {"path": "window_rule", "type": "array_of_tables"},
            {"path": "window_rule[].floating", "type": "bool"},
            {"path": "input.touchpad.accel_profile", "type": "enum_or_array", "values": ["flat", "adaptive"]},
            {"path": "environment", "type": "table"},
            {"path": "environment.<name>", "type": "string"}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    fn entry(key: &str) -> Json {
        schema_entry(&options(), key).unwrap().clone()
    }

    fn rendered(entry_key: &str, value: Json) -> String {
        toml_value(&entry(entry_key), &value)
            .unwrap()
            .to_string()
            .trim()
            .to_owned()
    }

    #[test]
    fn keys_are_matched_against_the_schema_with_name_placeholders() {
        assert_eq!(entry("layout.gap")["path"], "layout.gap");
        assert_eq!(
            entry("output.HDMI-A-1.scale")["path"],
            "output.<name>.scale"
        );
        assert_eq!(entry("environment.GTK_THEME")["path"], "environment.<name>");
    }

    #[test]
    fn unknown_keys_and_non_settings_are_refused_with_a_reason() {
        let error = |key: &str| schema_entry(&options(), key).unwrap_err().to_string();

        assert!(
            error("layout.gaps").contains("not an Umbriel setting"),
            "{}",
            error("layout.gaps")
        );
        assert!(error("layout").contains("a section"), "{}", error("layout"));
        assert!(
            error("window_rule").contains("rule list"),
            "{}",
            error("window_rule")
        );
        assert!(error("window_rule[0].floating").contains("rule list"));
    }

    #[test]
    fn values_are_checked_against_type_range_and_allowed_values() {
        assert_eq!(rendered("layout.gap", json!(12)), "12");
        assert_eq!(rendered("layout.gap", json!("12")), "12");
        assert_eq!(rendered("appearance.blur.enabled", json!(false)), "false");
        assert_eq!(rendered("appearance.blur.enabled", json!("true")), "true");
        assert_eq!(rendered("appearance.blur.radius", json!(4)), "4.0");
        assert_eq!(
            rendered("animation.workspaces.style", json!("reveal")),
            "\"reveal\""
        );
        assert_eq!(
            rendered("colors.border.active", json!("#89b4fa")),
            "\"#89b4fa\""
        );
        assert_eq!(
            rendered("general.autostart", json!(["noctalia", "kitty"])),
            "[\"noctalia\", \"kitty\"]"
        );
        assert_eq!(
            rendered("input.touchpad.accel_profile", json!("flat")),
            "\"flat\""
        );

        let refused =
            |key: &str, value: Json| toml_value(&entry(key), &value).unwrap_err().to_string();
        assert!(refused("layout.gap", json!(501)).contains("between 0 and 500"));
        assert!(refused("layout.gap", json!(1.5)).contains("whole number"));
        assert!(refused("appearance.blur.enabled", json!("yes")).contains("true or false"));
        assert!(refused("animation.workspaces.style", json!("spin")).contains("slide, reveal"));
        assert!(refused("general.autostart", json!("kitty")).contains("list of strings"));
        assert!(refused("output.DP-1.scale", json!(9)).contains("between 0.25 and 8"));
    }

    #[test]
    fn risky_keys_are_classified() {
        assert_eq!(policy("layout.gap"), Policy::Ordinary);
        for key in [
            "general.autostart",
            "events.lid_close",
            "environment.LD_PRELOAD",
            "include.files",
            "include.optional.files",
            "effects.preset.glow.shader",
            "drm.ignored_devices",
        ] {
            assert_eq!(policy(key), Policy::RunsProgramsOrLoadsFiles, "{key}");
        }
        assert_eq!(
            policy("screencast.disable_dynamic_confirmation"),
            Policy::WeakensSecurity
        );
        assert_eq!(policy("hot_corners.top_left.action"), Policy::TakesAnAction);
        assert_eq!(policy("keybinds.Mod+T"), Policy::UseKeybindTools);
        assert!(matches!(
            policy("input.keyboard.layout"),
            Policy::MayLockYouOut(_)
        ));
        assert!(matches!(
            policy("general.mod_key"),
            Policy::MayLockYouOut(_)
        ));
    }

    const CONFIG: &str = "# mine\n[layout]\ngap = 8   # tight\n\n[appearance]\nshadows = true\n";

    #[test]
    fn an_existing_setting_is_replaced_in_place_keeping_its_comment() {
        let planned = plan_set(CONFIG, "layout.gap", Value::from(12)).unwrap();

        assert_eq!(planned.previous.as_deref(), Some("8"));
        assert!(
            planned.new_text.contains("gap = 12   # tight"),
            "{}",
            planned.new_text
        );
        assert!(planned.new_text.starts_with("# mine\n[layout]\n"));
    }

    #[test]
    fn a_new_setting_goes_into_its_existing_section() {
        let planned = plan_set(CONFIG, "layout.smart_gaps", Value::from(true)).unwrap();

        assert_eq!(planned.previous, None);
        assert!(
            planned
                .new_text
                .contains("gap = 8   # tight\nsmart_gaps = true\n"),
            "{}",
            planned.new_text
        );
    }

    #[test]
    fn a_setting_in_a_missing_section_creates_only_that_section() {
        let planned = plan_set(CONFIG, "output.HDMI-A-1.scale", Value::from(1.25)).unwrap();

        assert!(
            planned.new_text.contains("[output.HDMI-A-1]\nscale = 1.25"),
            "{}",
            planned.new_text
        );
        assert!(
            !planned.new_text.contains("[output]\n"),
            "{}",
            planned.new_text
        );
        assert!(planned.new_text.starts_with(CONFIG));
    }

    #[test]
    fn a_value_where_a_section_is_expected_is_refused() {
        let error = plan_set("layout = 3\n", "layout.gap", Value::from(1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("layout"), "{error}");
    }

    #[test]
    fn resetting_removes_the_setting_so_the_default_applies() {
        let planned = plan_reset(CONFIG, "layout.gap").unwrap();

        assert_eq!(planned.previous.as_deref(), Some("8"));
        assert!(!planned.new_text.contains("gap"), "{}", planned.new_text);
        assert!(planned.new_text.contains("[layout]"));
        let error = plan_reset(CONFIG, "layout.smart_gaps")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not set"), "{error}");
    }

    #[test]
    fn disabling_the_last_enabled_output_is_detected() {
        let both = vec!["HDMI-A-1".to_owned(), "DP-1".to_owned()];
        let one = vec!["DP-1".to_owned()];
        let off = Value::from(false);

        assert!(!would_disable_every_output(
            &both,
            "output.DP-1.enabled",
            &off
        ));
        assert!(would_disable_every_output(
            &one,
            "output.DP-1.enabled",
            &off
        ));
        assert!(!would_disable_every_output(
            &one,
            "output.DP-1.enabled",
            &Value::from(true)
        ));
        assert!(!would_disable_every_output(
            &one,
            "output.HDMI-A-1.enabled",
            &off
        ));
        assert!(!would_disable_every_output(&one, "layout.gap", &off));
    }
}
