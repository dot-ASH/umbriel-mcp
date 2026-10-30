use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const ACTION_LIST_HEADER: &str = "Available actions";
const MAXIMUM_LISTED_OPTIONS: usize = 60;
const PACKAGED_CONFIG_PATHS: [&str; 2] = [
    "/usr/share/umbriel/config.toml",
    "/usr/local/share/umbriel/config.toml",
];
const REFERENCE_URL: &str = "https://docs.noctalia.dev/umbriel/";

#[derive(Debug, PartialEq, Eq)]
pub struct Action {
    pub group: String,
    pub usage: String,
    pub description: String,
}

pub fn parse_actions(help: &str) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut group = None;
    for line in help
        .lines()
        .skip_while(|line| !line.starts_with(ACTION_LIST_HEADER))
        .skip(1)
    {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            group = Some(line.trim().to_owned());
            continue;
        }
        let Some(group) = &group else { continue };
        let entry = line.trim();
        let (usage, description) = entry
            .split_once("  ")
            .map(|(usage, description)| (usage.trim(), description.trim()))
            .unwrap_or((entry, ""));
        actions.push(Action {
            group: group.clone(),
            usage: usage.to_owned(),
            description: description.to_owned(),
        });
    }
    actions
}

pub fn actions_matching<'a>(actions: &'a [Action], filter: Option<&str>) -> Vec<&'a Action> {
    let wanted = filter.map(str::trim).unwrap_or_default().to_lowercase();
    actions
        .iter()
        .filter(|action| {
            wanted.is_empty()
                || [&action.usage, &action.description, &action.group]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&wanted))
        })
        .collect()
}

fn option_path(option: &Value) -> &str {
    option
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

pub fn options_matching(schema: &Value, query: Option<&str>) -> Result<Value> {
    let options = schema
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("the Umbriel config schema has no option list"))?;
    let version = schema.get("version").cloned().unwrap_or(Value::Null);
    let words: Vec<String> = query
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();

    if words.is_empty() {
        let mut sections = BTreeMap::new();
        for option in options {
            let section = option_path(option)
                .split(['.', '['])
                .next()
                .unwrap_or_default()
                .to_owned();
            *sections.entry(section).or_insert(0) += 1;
        }
        return Ok(json!({
            "umbriel_version": version,
            "sections": sections,
            "hint": "Pass a query such as \"gap\" or \"blur\" to list matching options with type, default and allowed values.",
        }));
    }

    let matching: Vec<&Value> = options
        .iter()
        .filter(|option| {
            let path = option_path(option).to_lowercase();
            words.iter().all(|word| path.contains(word.as_str()))
        })
        .collect();
    let mut result = Map::new();
    result.insert("umbriel_version".into(), version);
    result.insert("total_matches".into(), json!(matching.len()));
    result.insert(
        "options".into(),
        Value::Array(
            matching
                .iter()
                .take(MAXIMUM_LISTED_OPTIONS)
                .map(|option| (*option).clone())
                .collect(),
        ),
    );
    if matching.len() > MAXIMUM_LISTED_OPTIONS {
        result.insert(
            "note".into(),
            json!(format!(
                "showing the first {MAXIMUM_LISTED_OPTIONS}; narrow the query to see the rest"
            )),
        );
    }
    result.insert(
        "descriptions".into(),
        json!(format!(
            "The schema has no prose descriptions; see {REFERENCE_URL} for what each option does."
        )),
    );
    Ok(Value::Object(result))
}

fn section_name_of(line: &str) -> Option<&str> {
    let line = line.trim();
    let inner = line
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .or_else(|| {
            line.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
        })?;
    Some(inner.trim())
}

fn belongs_to(section_name: &str, wanted: &str) -> bool {
    section_name == wanted
        || section_name
            .strip_prefix(wanted)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub fn config_section(config: &str, section: &str) -> Option<String> {
    let wanted = section.trim();
    let mut kept = Vec::new();
    let mut inside = false;
    for line in config.lines() {
        if let Some(name) = section_name_of(line) {
            inside = belongs_to(name, wanted);
        }
        if inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    (!kept.is_empty()).then(|| kept.join("\n"))
}

pub fn user_config_path(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|path| !path.is_empty()) {
        Some(config_home) => PathBuf::from(config_home),
        None => PathBuf::from(home.filter(|path| !path.is_empty())?).join(".config"),
    };
    Some(base.join("umbriel").join("config.toml"))
}

fn run_umbriel(arguments: &[&str]) -> Result<String> {
    let output = Command::new("umbriel")
        .args(arguments)
        .output()
        .map_err(|cause| {
            anyhow!("cannot run the umbriel command ({cause}); is Umbriel installed and on PATH?")
        })?;
    let text = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
    if !output.status.success() {
        return Err(anyhow!(
            "`umbriel {}` failed: {}",
            arguments.join(" "),
            text.trim()
        ));
    }
    Ok(text)
}

fn query_or_error(command: &str) -> Value {
    crate::ipc::query(command).unwrap_or_else(|error| json!({ "error": format!("{error:#}") }))
}

pub fn info() -> Value {
    let version = run_umbriel(&["--version"])
        .map(|text| Value::String(text.trim().to_owned()))
        .unwrap_or_else(|error| json!({ "error": format!("{error:#}") }));
    json!({
        "installed_version": version,
        "keyboard_layouts": query_or_error("keyboard-layouts"),
        "active_submap": query_or_error("submap"),
        "color_management": query_or_error("color"),
        "tearing": query_or_error("tearing"),
        "reference": REFERENCE_URL,
    })
}

pub fn list_actions(filter: Option<&str>) -> Result<String> {
    let actions = parse_actions(&run_umbriel(&["msg", "--help"])?);
    if actions.is_empty() {
        return Err(anyhow!("`umbriel msg --help` listed no actions"));
    }
    let matching = actions_matching(&actions, filter);
    if matching.is_empty() {
        return Ok(format!(
            "no action matches {:?}; call list_actions without a filter to see all {}",
            filter.unwrap_or_default(),
            actions.len()
        ));
    }
    let mut listing = format!(
        "{} of {} actions. Run one with run_action, filling in <placeholders> and dropping [optional] parts you do not need.\n",
        matching.len(),
        actions.len()
    );
    let mut current_group = "";
    for action in matching {
        if action.group != current_group {
            current_group = &action.group;
            listing.push_str(&format!("\n{current_group}\n"));
        }
        listing.push_str(&format!("  {}  {}\n", action.usage, action.description));
    }
    Ok(listing)
}

pub fn config_options(query: Option<&str>) -> Result<Value> {
    let schema: Value = serde_json::from_str(&run_umbriel(&["config", "schema", "--json"])?)
        .context("`umbriel config schema --json` did not print JSON")?;
    options_matching(&schema, query)
}

fn existing_config() -> Option<(PathBuf, bool)> {
    let user = user_config_path(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    );
    if let Some(path) = user.filter(|path| path.is_file()) {
        return Some((path, true));
    }
    PACKAGED_CONFIG_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(|path| (path.to_path_buf(), false))
}

pub fn read_config(section: Option<&str>) -> Result<String> {
    let (path, is_user_config) = existing_config().ok_or_else(|| {
        anyhow!("no Umbriel config found in ~/.config/umbriel/config.toml or the packaged defaults")
    })?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let origin = if is_user_config {
        format!("Config: {}", path.display())
    } else {
        format!(
            "No user config; Umbriel is using its packaged config {}",
            path.display()
        )
    };
    let validation = run_umbriel(&["config", "validate"])
        .map(|report| report.trim().to_owned())
        .unwrap_or_else(|error| format!("{error:#}"));
    let body = match section.map(str::trim).filter(|section| !section.is_empty()) {
        Some(section) => config_section(&text, section)
            .ok_or_else(|| anyhow!("{} has no [{section}] section", path.display()))?,
        None => text,
    };
    Ok(format!(
        "{origin}\nValidation: {validation}\nFiles listed under [include] are not shown here.\n\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        actions_matching, config_section, options_matching, parse_actions, user_config_path,
    };
    use serde_json::json;
    use std::path::PathBuf;

    const HELP: &str = "Usage: umbriel msg <action> [args...]

Send an action to the running compositor.
Use `msg spawn <cmd...>` as shorthand for `msg spawn:<cmd...>`.
Available actions, grouped as the cheatsheet groups them:

Apps
  spawn:<cmd>  Run a command with a launch activation token

Focus
  output-focus-left               Focus the output to the left
  window-focus:<window-id>        Focus a window, revealing it from a hidden scratchpad

Workspaces
  workspace-switch:<selector>  Switch to a workspace
";

    const CONFIG: &str = "# Umbriel config
[general]
autostart = [\"noctalia\"]

[layout]
gap = 8

[layout.dwindle]
split = 0.5

[keybinds]
\"Mod+T\" = { action = \"spawn:kitty\" }
\"Mod+Q\" = { action = \"window-close\" }

[[window_rule]]
match = { app_id = \"pavucontrol\" }
floating = true

[[window_rule]]
match = { app_id = \"mpv\" }
";

    #[test]
    fn actions_are_parsed_with_their_group_usage_and_description() {
        let actions = parse_actions(HELP);

        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0].group, "Apps");
        assert_eq!(actions[0].usage, "spawn:<cmd>");
        assert_eq!(
            actions[0].description,
            "Run a command with a launch activation token"
        );
        assert_eq!(actions[2].group, "Focus");
        assert_eq!(actions[2].usage, "window-focus:<window-id>");
        assert_eq!(actions[3].group, "Workspaces");
    }

    #[test]
    fn the_usage_preamble_is_not_mistaken_for_actions() {
        let actions = parse_actions(HELP);

        assert!(actions.iter().all(|action| !action.usage.contains("Usage")));
        assert!(
            actions
                .iter()
                .all(|action| !action.group.contains("Available"))
        );
    }

    #[test]
    fn actions_can_be_filtered_by_name_description_or_group() {
        let actions = parse_actions(HELP);

        let by_name: Vec<&str> = actions_matching(&actions, Some("FOCUS"))
            .iter()
            .map(|action| action.usage.as_str())
            .collect();
        assert_eq!(by_name, ["output-focus-left", "window-focus:<window-id>"]);
        assert_eq!(actions_matching(&actions, Some("scratchpad")).len(), 1);
        assert_eq!(actions_matching(&actions, Some("workspaces")).len(), 1);
        assert_eq!(actions_matching(&actions, None).len(), 4);
        assert!(actions_matching(&actions, Some("teleport")).is_empty());
    }

    fn schema() -> serde_json::Value {
        json!({
            "version": "0.1.0",
            "revision": "abc",
            "options": [
                {"path": "layout", "type": "table"},
                {"path": "layout.gap", "type": "int", "min": 0, "max": 500, "default": 8},
                {"path": "appearance.tab_bar.gap", "type": "int", "default": -1},
                {"path": "appearance.blur.enabled", "type": "bool", "default": true},
                {"path": "keybinds", "type": "map"},
                {"path": "layout.dwindle.split", "type": "float", "default": 0.5}
            ]
        })
    }

    #[test]
    fn option_search_matches_every_word_of_the_query_in_the_path() {
        let found = options_matching(&schema(), Some("gap")).unwrap();
        let paths: Vec<&str> = found["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["layout.gap", "appearance.tab_bar.gap"]);

        let found = options_matching(&schema(), Some("blur enabled")).unwrap();
        assert_eq!(found["options"][0]["default"], json!(true));
        assert_eq!(found["options"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn option_search_reports_the_schema_version() {
        let found = options_matching(&schema(), Some("gap")).unwrap();
        assert_eq!(found["umbriel_version"], "0.1.0");
    }

    #[test]
    fn without_a_query_the_option_search_summarises_sections() {
        let summary = options_matching(&schema(), None).unwrap();
        assert_eq!(
            summary["sections"],
            json!({"appearance": 2, "keybinds": 1, "layout": 3})
        );
        assert!(summary.get("options").is_none());
    }

    #[test]
    fn a_large_option_search_is_capped_with_a_note() {
        let options: Vec<_> = (0..200)
            .map(|index| json!({"path": format!("layout.item{index}"), "type": "int"}))
            .collect();
        let schema = json!({"version": "0.1.0", "options": options});

        let found = options_matching(&schema, Some("layout")).unwrap();

        assert!(found["options"].as_array().unwrap().len() <= 60);
        assert_eq!(found["total_matches"], 200);
    }

    #[test]
    fn a_section_includes_its_subsections_and_stops_at_the_next_section() {
        let layout = config_section(CONFIG, "layout").unwrap();

        assert!(layout.starts_with("[layout]"));
        assert!(layout.contains("gap = 8"));
        assert!(layout.contains("[layout.dwindle]"));
        assert!(!layout.contains("keybinds"));
    }

    #[test]
    fn a_dotted_section_name_selects_only_that_subsection() {
        let dwindle = config_section(CONFIG, "layout.dwindle").unwrap();

        assert!(dwindle.starts_with("[layout.dwindle]"));
        assert!(!dwindle.contains("gap = 8"));
    }

    #[test]
    fn array_of_tables_sections_return_every_entry() {
        let rules = config_section(CONFIG, "window_rule").unwrap();

        assert_eq!(rules.matches("[[window_rule]]").count(), 2);
        assert!(rules.contains("pavucontrol") && rules.contains("mpv"));
    }

    #[test]
    fn keybinds_are_returned_verbatim() {
        let keybinds = config_section(CONFIG, "keybinds").unwrap();

        assert!(keybinds.contains("\"Mod+T\" = { action = \"spawn:kitty\" }"));
        assert!(!keybinds.contains("window_rule"));
    }

    #[test]
    fn a_missing_section_is_none_and_a_prefix_is_not_a_match() {
        assert!(config_section(CONFIG, "hot_corners").is_none());
        assert!(config_section(CONFIG, "lay").is_none());
    }

    #[test]
    fn the_user_config_path_follows_xdg_then_home() {
        assert_eq!(
            user_config_path(Some("/x/config"), Some("/home/u")),
            Some(PathBuf::from("/x/config/umbriel/config.toml"))
        );
        assert_eq!(
            user_config_path(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/umbriel/config.toml"))
        );
        assert_eq!(
            user_config_path(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/umbriel/config.toml"))
        );
        assert_eq!(user_config_path(None, None), None);
    }
}
