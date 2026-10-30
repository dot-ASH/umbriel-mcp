use clap::ValueEnum;
use clap::builder::PossibleValue;
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Profile {
    ReadOnly,
    NoExec,
    #[default]
    Full,
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self
            .to_possible_value()
            .as_ref()
            .map(PossibleValue::get_name)
            .map(str::to_owned)
            .unwrap_or_default();
        formatter.write_str(&name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Observe,
    Control,
    Execute,
}

impl Access {
    fn weakest_permitting_profile(self) -> Profile {
        match self {
            Access::Observe => Profile::ReadOnly,
            Access::Control => Profile::NoExec,
            Access::Execute => Profile::Full,
        }
    }
}

pub const TOOL_ACCESS: &[(&str, Access)] = &[
    ("list_windows", Access::Observe),
    ("list_workspaces", Access::Observe),
    ("list_outputs", Access::Observe),
    ("get_active_window", Access::Observe),
    ("list_layers", Access::Observe),
    ("screenshot", Access::Observe),
    ("screenshot_with_ocr", Access::Observe),
    ("find_text_on_screen", Access::Observe),
    ("clipboard_read", Access::Observe),
    ("umbriel_info", Access::Observe),
    ("list_actions", Access::Observe),
    ("config_options", Access::Observe),
    ("read_config", Access::Observe),
    ("switch_workspace", Access::Control),
    ("focus_window", Access::Control),
    ("close_window", Access::Control),
    ("kill_window", Access::Control),
    ("move_window_to_workspace", Access::Control),
    ("toggle_fullscreen", Access::Control),
    ("toggle_floating", Access::Control),
    ("resize_window", Access::Control),
    ("set_workspace_layout", Access::Control),
    ("toggle_overview", Access::Control),
    ("run_action", Access::Control),
    ("mouse_move", Access::Control),
    ("mouse_click", Access::Control),
    ("mouse_scroll", Access::Control),
    ("mouse_drag", Access::Control),
    ("type_text", Access::Control),
    ("key_press", Access::Control),
    ("click_text", Access::Control),
    ("clipboard_write", Access::Control),
    ("set_keybind", Access::Control),
    ("remove_keybind", Access::Control),
    ("launch_app", Access::Execute),
];

fn access_for_tool(tool: &str) -> Access {
    TOOL_ACCESS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, access)| *access)
        .unwrap_or(Access::Execute)
}

pub fn is_spawn_action(action: &str) -> bool {
    let action_name = action
        .trim()
        .split(|character: char| character == ':' || character.is_whitespace())
        .next()
        .unwrap_or_default();
    action_name.to_ascii_lowercase().starts_with("spawn")
}

impl Profile {
    pub fn permit_tool(self, tool: &str) -> Result<(), String> {
        let required = access_for_tool(tool).weakest_permitting_profile();
        if self >= required {
            return Ok(());
        }
        Err(format!(
            "`{tool}` is not available in the `{self}` profile; it needs the `{required}` \
             profile. Restart umbriel-mcp with `--profile {required}` to allow it."
        ))
    }

    pub fn permit_action(self, tool: &str, action: &str) -> Result<(), String> {
        if self < Profile::Full && is_spawn_action(action) {
            return Err(format!(
                "`{tool}` cannot use `{}` in the `{self}` profile: actions that start \
                 programs need the `full` profile. Restart umbriel-mcp with `--profile full` \
                 to allow it.",
                action.trim()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Profile, TOOL_ACCESS, is_spawn_action};

    #[test]
    fn profiles_are_ordered_from_least_to_most_capable() {
        assert!(Profile::ReadOnly < Profile::NoExec);
        assert!(Profile::NoExec < Profile::Full);
        assert_eq!(Profile::default(), Profile::Full);
    }

    #[test]
    fn profiles_display_the_names_used_on_the_command_line() {
        assert_eq!(Profile::ReadOnly.to_string(), "read-only");
        assert_eq!(Profile::NoExec.to_string(), "no-exec");
        assert_eq!(Profile::Full.to_string(), "full");
    }

    #[test]
    fn spawn_family_is_recognised_in_every_spelling_the_compositor_might_accept() {
        for action in [
            "spawn:kitty",
            "spawn",
            "SPAWN:kitty",
            "Spawn:kitty",
            "  spawn:kitty",
            "\tspawn:kitty",
            "spawn kitty",
            "spawn-later:kitty",
        ] {
            assert!(
                is_spawn_action(action),
                "{action:?} should be a spawn action"
            );
        }
    }

    #[test]
    fn ordinary_actions_are_not_mistaken_for_spawn() {
        for action in [
            "overview-toggle",
            "workspace-switch:2/DP-1",
            "window-close:abc",
            "window-focus:spawn-window-id",
            "",
        ] {
            assert!(
                !is_spawn_action(action),
                "{action:?} should not be a spawn action"
            );
        }
    }

    #[test]
    fn unknown_tools_are_treated_as_needing_the_full_profile() {
        assert!(Profile::NoExec.permit_tool("a_tool_added_later").is_err());
        assert!(Profile::ReadOnly.permit_tool("a_tool_added_later").is_err());
        assert!(Profile::Full.permit_tool("a_tool_added_later").is_ok());
    }

    #[test]
    fn each_tool_is_classified_once() {
        let mut names: Vec<&str> = TOOL_ACCESS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "a tool appears twice in TOOL_ACCESS");
    }
}
