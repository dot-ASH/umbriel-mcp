use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct CompositorAction {
    name: String,
    fields: Map<String, Value>,
    legacy_argument: Option<String>,
}

fn workspace_field(workspace: &str) -> Value {
    let workspace = workspace.trim();
    match workspace.parse::<u64>() {
        Ok(position) if !workspace.is_empty() => json!(position),
        _ => json!(workspace),
    }
}

impl CompositorAction {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            fields: Map::new(),
            legacy_argument: None,
        }
    }

    fn with(mut self, field: &str, value: Value, legacy_argument: String) -> Self {
        self.fields.insert(field.to_owned(), value);
        self.legacy_argument = Some(legacy_argument);
        self
    }

    pub fn plain(name: &str) -> Self {
        Self::new(name)
    }

    pub fn on_workspace(name: &str, workspace: &str, output: Option<&str>) -> Self {
        let legacy = match output {
            Some(output) => format!("{}/{output}", workspace.trim()),
            None => workspace.trim().to_owned(),
        };
        let mut action = Self::new(name).with("workspace", workspace_field(workspace), legacy);
        if let Some(output) = output {
            action.fields.insert("output".to_owned(), json!(output));
        }
        action
    }

    pub fn on_window(name: &str, window_id: Option<&str>) -> Self {
        match window_id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => Self::new(name).with("window", json!(id), id.to_owned()),
            None => Self::new(name),
        }
    }

    pub fn by_delta(name: &str, delta: f64) -> Self {
        Self::new(name).with("delta", json!(delta), delta.to_string())
    }

    pub fn set_layout(mode: &str) -> Self {
        let mode = mode.trim().to_lowercase();
        let typed = if mode == "floating" {
            "stacking"
        } else {
            &mode
        };
        let legacy = if mode == "stacking" {
            "floating"
        } else {
            &mode
        };
        Self::new("workspace-set-layout").with("layout", json!(typed), legacy.to_owned())
    }

    pub fn spawn(command: &str) -> Self {
        Self::new("spawn").with("command", json!(command), command.to_owned())
    }

    pub fn typed_request(&self) -> Value {
        let mut request = Map::new();
        request.insert("cmd".to_owned(), json!("action"));
        request.insert("action".to_owned(), json!(self.name));
        request.extend(self.fields.clone());
        Value::Object(request)
    }

    pub fn summary(&self) -> String {
        let mut words = vec![self.name.clone()];
        for (field, value) in &self.fields {
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            if field == "output" {
                words.push("--output".to_owned());
            }
            words.push(value);
        }
        words.join(" ")
    }

    pub fn legacy_text(&self) -> String {
        match &self.legacy_argument {
            Some(argument) => format!("{}:{argument}", self.name),
            None => self.name.clone(),
        }
    }
}

pub fn split_words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Some(open), close) if close == open => quote = None,
            (Some('"') | None, '\\') => {
                let escaped = characters
                    .next()
                    .ok_or_else(|| anyhow!("the action ends with a lone backslash"))?;
                current.push(escaped);
                in_word = true;
            }
            (Some(_), other) => current.push(other),
            (None, '\'' | '"') => {
                quote = Some(character);
                in_word = true;
            }
            (None, blank) if blank.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, other) => {
                current.push(other);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(anyhow!("the action has an unclosed quote"));
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

const WORKSPACE_SELECTING_ACTIONS: &[&str] = &["workspace-switch", "window-move-to-workspace"];

fn legacy_form(text: &str) -> Option<(&str, &str)> {
    let first_word = text.split_whitespace().next()?;
    let (name, _) = first_word.split_once(':')?;
    let looks_like_an_action_name = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
    looks_like_an_action_name.then(|| (name, &text[name.len() + 1..]))
}

pub fn words_for_command_line(text: &str) -> Result<Vec<String>> {
    let text = text.trim();
    if let Some((name, argument)) = legacy_form(text) {
        let argument = argument.trim();
        let mut words = vec![name.to_owned()];
        match argument.split_once('/') {
            Some((workspace, output)) if WORKSPACE_SELECTING_ACTIONS.contains(&name) => {
                words.extend([
                    workspace.to_owned(),
                    "--output".to_owned(),
                    output.to_owned(),
                ]);
            }
            _ if argument.is_empty() => {}
            _ => words.push(argument.to_owned()),
        }
        return Ok(words);
    }
    if let Some(command) = text
        .strip_prefix("spawn")
        .filter(|rest| rest.starts_with(char::is_whitespace))
    {
        return Ok(vec!["spawn".to_owned(), command.trim().to_owned()]);
    }
    let words = split_words(text)?;
    if words.is_empty() {
        return Err(anyhow!("the action is empty"));
    }
    Ok(words)
}

pub fn legacy_text_for(text: &str) -> Result<String> {
    let text = text.trim();
    if legacy_form(text).is_some() {
        return Ok(text.to_owned());
    }
    let words = words_for_command_line(text)?;
    let (name, arguments) = words.split_first().expect("at least one word");
    if arguments.is_empty() {
        return Ok(name.clone());
    }
    let mut argument = String::new();
    let mut remaining = arguments.iter();
    while let Some(word) = remaining.next() {
        if word == "--output" {
            let output = remaining
                .next()
                .ok_or_else(|| anyhow!("--output needs an output name after it"))?;
            argument.push('/');
            argument.push_str(output);
        } else {
            if !argument.is_empty() {
                argument.push(' ');
            }
            argument.push_str(word);
        }
    }
    Ok(format!("{name}:{argument}"))
}

#[cfg(test)]
mod tests {
    use super::{CompositorAction, legacy_text_for, split_words, words_for_command_line};
    use serde_json::json;

    #[test]
    fn workspace_actions_send_positions_as_numbers_and_names_as_text() {
        let switch = CompositorAction::on_workspace("workspace-switch", "2", Some("DP-1"));
        assert_eq!(
            switch.typed_request(),
            json!({"cmd": "action", "action": "workspace-switch", "workspace": 2, "output": "DP-1"})
        );
        assert_eq!(switch.legacy_text(), "workspace-switch:2/DP-1");
        let named = CompositorAction::on_workspace("workspace-switch", "web", None);
        assert_eq!(named.typed_request()["workspace"], "web");
        assert_eq!(named.legacy_text(), "workspace-switch:web");
    }

    #[test]
    fn window_actions_name_the_window_only_when_given() {
        let focus = CompositorAction::on_window("window-focus", Some("abc"));
        assert_eq!(
            focus.typed_request(),
            json!({"cmd": "action", "action": "window-focus", "window": "abc"})
        );
        assert_eq!(focus.legacy_text(), "window-focus:abc");
        let close = CompositorAction::on_window("window-close", None);
        assert_eq!(
            close.typed_request(),
            json!({"cmd": "action", "action": "window-close"})
        );
        assert_eq!(close.legacy_text(), "window-close");
    }

    #[test]
    fn the_floating_layout_is_called_stacking_by_newer_umbriel() {
        assert_eq!(
            CompositorAction::set_layout("floating").typed_request()["layout"],
            "stacking"
        );
        assert_eq!(
            CompositorAction::set_layout("stacking").legacy_text(),
            "workspace-set-layout:floating"
        );
        assert_eq!(
            CompositorAction::set_layout("dwindle").legacy_text(),
            "workspace-set-layout:dwindle"
        );
    }

    #[test]
    fn deltas_and_commands_keep_their_values() {
        let resize = CompositorAction::by_delta("window-modify-width-left", -0.1);
        assert_eq!(resize.typed_request()["delta"], json!(-0.1));
        assert_eq!(resize.legacy_text(), "window-modify-width-left:-0.1");
        let spawn = CompositorAction::spawn("sh -c 'echo hi'");
        assert_eq!(spawn.typed_request()["command"], "sh -c 'echo hi'");
        assert_eq!(spawn.legacy_text(), "spawn:sh -c 'echo hi'");
    }

    #[test]
    fn words_split_like_config_strings() {
        assert_eq!(
            split_words(r#"workspace-switch 'My web' --output "DP 1" a\ b"#).unwrap(),
            ["workspace-switch", "My web", "--output", "DP 1", "a b"]
        );
        assert!(split_words("workspace-switch 'open").is_err());
    }

    fn words(text: &str) -> Vec<String> {
        words_for_command_line(text).unwrap()
    }

    #[test]
    fn either_syntax_becomes_command_line_words() {
        assert_eq!(
            words("workspace-switch 2 --output DP-1"),
            ["workspace-switch", "2", "--output", "DP-1"]
        );
        assert_eq!(
            words("workspace-switch:2/DP-1"),
            ["workspace-switch", "2", "--output", "DP-1"]
        );
        assert_eq!(words("window-focus:abc"), ["window-focus", "abc"]);
        assert_eq!(words("overview-toggle"), ["overview-toggle"]);
        assert_eq!(words("spawn sh -c 'echo hi'"), ["spawn", "sh -c 'echo hi'"]);
        assert_eq!(words("spawn:kitty --hold"), ["spawn", "kitty --hold"]);
        assert!(words_for_command_line("   ").is_err());
    }

    #[test]
    fn either_syntax_becomes_the_older_name_colon_argument_form() {
        assert_eq!(
            legacy_text_for("workspace-switch 2 --output DP-1").unwrap(),
            "workspace-switch:2/DP-1"
        );
        assert_eq!(
            legacy_text_for("window-focus abc").unwrap(),
            "window-focus:abc"
        );
        assert_eq!(
            legacy_text_for("window-focus:abc").unwrap(),
            "window-focus:abc"
        );
        assert_eq!(
            legacy_text_for("overview-toggle").unwrap(),
            "overview-toggle"
        );
        assert_eq!(
            legacy_text_for("spawn kitty --hold").unwrap(),
            "spawn:kitty --hold"
        );
    }
}
