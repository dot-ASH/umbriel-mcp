use crate::profile::Profile;
use anyhow::{Result, anyhow};
use clap::ValueEnum;
use serde_json::json;
use std::process::{Command, ExitCode};

const SERVER_NAME: &str = "umbriel";
const SERVER_BINARY: &str = "umbriel-mcp";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Pi,
    ClaudeCode,
    Vscode,
    Opencode,
    Cursor,
    Zed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Registration {
    Delegate {
        program: &'static str,
        arguments: Vec<String>,
        follow_up: &'static str,
    },
    Snippet {
        file: &'static str,
        text: String,
        follow_up: &'static str,
    },
}

fn server_arguments(profile: Option<Profile>) -> Vec<String> {
    match profile {
        Some(profile) => vec!["--profile".to_owned(), profile.to_string()],
        None => Vec::new(),
    }
}

fn server_invocation(profile: Option<Profile>, server_command: &str) -> Vec<String> {
    std::iter::once(server_command.to_owned())
        .chain(server_arguments(profile))
        .collect()
}

fn pretty(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).expect("snippet is serializable")
}

pub fn plan(harness: Harness, profile: Option<Profile>, server_command: &str) -> Registration {
    let invocation = server_invocation(profile, server_command);
    let words = |prefix: &[&str]| -> Vec<String> {
        prefix
            .iter()
            .map(|word| (*word).to_owned())
            .chain(invocation.clone())
            .collect()
    };
    match harness {
        Harness::Pi => Registration::Delegate {
            program: "pi",
            arguments: words(&["mcp", "add", SERVER_NAME, "--exposure", "direct", "--"]),
            follow_up: "Start a new pi session; the tools are exposed directly to the model.",
        },
        Harness::ClaudeCode => Registration::Delegate {
            program: "claude",
            arguments: words(&["mcp", "add", "--scope", "user", SERVER_NAME, "--"]),
            follow_up: "Start a new Claude Code session and check `/mcp` lists umbriel.",
        },
        Harness::Vscode => Registration::Delegate {
            program: "code",
            arguments: vec![
                "--add-mcp".to_owned(),
                json!({
                    "name": SERVER_NAME,
                    "command": server_command,
                    "args": server_arguments(profile),
                })
                .to_string(),
            ],
            follow_up: "Reload the VS Code window and start the server from the MCP servers list.",
        },
        Harness::Opencode => Registration::Snippet {
            file: "opencode.json (project) or ~/.config/opencode/opencode.json",
            text: pretty(json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": { SERVER_NAME: {
                    "type": "local",
                    "command": invocation,
                    "enabled": true,
                }},
            })),
            follow_up: "Merge the \"umbriel\" entry into the file's \"mcp\" object, then restart opencode.",
        },
        Harness::Cursor => Registration::Snippet {
            file: "~/.cursor/mcp.json (all projects) or .cursor/mcp.json (one project)",
            text: pretty(json!({
                "mcpServers": { SERVER_NAME: {
                    "command": server_command,
                    "args": server_arguments(profile),
                }},
            })),
            follow_up: "Merge the \"umbriel\" entry into \"mcpServers\", then enable it under Cursor Settings > MCP.",
        },
        Harness::Zed => Registration::Snippet {
            file: "Zed settings.json (open it with the `zed: open settings` command)",
            text: pretty(json!({
                "context_servers": { SERVER_NAME: {
                    "command": server_command,
                    "args": server_arguments(profile),
                }},
            })),
            follow_up: "Merge the \"umbriel\" entry into \"context_servers\"; keep \"args\" even when empty or Zed rejects the whole file.",
        },
    }
}

fn is_on_path(program: &str) -> bool {
    crate::doctor::is_on_path(program)
}

fn server_command_for_registration() -> String {
    if is_on_path(SERVER_BINARY) {
        return SERVER_BINARY.to_owned();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| SERVER_BINARY.to_owned())
}

fn manual_command(program: &str, arguments: &[String]) -> String {
    let quoted: Vec<String> = arguments
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect();
    format!("{program} {}", quoted.join(" "))
}

fn execute(registration: Registration) -> Result<()> {
    match registration {
        Registration::Snippet {
            file,
            text,
            follow_up,
        } => {
            println!("Add this to {file}:\n\n{text}\n\n{follow_up}");
            println!("umbriel-mcp does not edit this file for you.");
            Ok(())
        }
        Registration::Delegate {
            program,
            arguments,
            follow_up,
        } => {
            if !is_on_path(program) {
                return Err(anyhow!(
                    "`{program}` was not found on PATH, so nothing was registered. \
                     Install it, or run this yourself later:\n  {}",
                    manual_command(program, &arguments)
                ));
            }
            let status = Command::new(program)
                .args(&arguments)
                .status()
                .map_err(|error| anyhow!("could not run `{program}`: {error}"))?;
            if !status.success() {
                return Err(anyhow!(
                    "`{program}` exited with {status}; registration was not completed"
                ));
            }
            println!("Registered umbriel with {program}.\n{follow_up}");
            Ok(())
        }
    }
}

pub fn run(harness: Harness, profile: Option<Profile>) -> ExitCode {
    let registration = plan(harness, profile, &server_command_for_registration());
    match execute(registration) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Harness, Registration, plan};
    use crate::profile::Profile;
    use serde_json::{Value, json};

    fn delegated(harness: Harness, profile: Option<Profile>) -> (&'static str, Vec<String>) {
        match plan(harness, profile, "umbriel-mcp") {
            Registration::Delegate {
                program, arguments, ..
            } => (program, arguments),
            other => panic!("{harness:?} should delegate, got {other:?}"),
        }
    }

    fn snippet(harness: Harness, profile: Option<Profile>) -> (&'static str, Value) {
        match plan(harness, profile, "umbriel-mcp") {
            Registration::Snippet { file, text, .. } => (
                file,
                serde_json::from_str(&text).expect("snippet is valid JSON"),
            ),
            other => panic!("{harness:?} should print a snippet, got {other:?}"),
        }
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn pi_is_registered_with_direct_exposure_so_tools_reach_the_model() {
        let (program, arguments) = delegated(Harness::Pi, None);
        assert_eq!(program, "pi");
        assert_eq!(
            arguments,
            strings(&[
                "mcp",
                "add",
                "umbriel",
                "--exposure",
                "direct",
                "--",
                "umbriel-mcp"
            ])
        );
    }

    #[test]
    fn claude_code_is_registered_for_the_user_across_projects() {
        let (program, arguments) = delegated(Harness::ClaudeCode, None);
        assert_eq!(program, "claude");
        assert_eq!(
            arguments,
            strings(&[
                "mcp",
                "add",
                "--scope",
                "user",
                "umbriel",
                "--",
                "umbriel-mcp"
            ])
        );
    }

    #[test]
    fn vscode_receives_the_server_definition_as_json() {
        let (program, arguments) = delegated(Harness::Vscode, None);
        assert_eq!(program, "code");
        assert_eq!(arguments[0], "--add-mcp");
        let definition: Value = serde_json::from_str(&arguments[1]).unwrap();
        assert_eq!(
            definition,
            json!({"name":"umbriel","command":"umbriel-mcp","args":[]})
        );
        assert_eq!(arguments.len(), 2);
    }

    #[test]
    fn a_profile_is_passed_through_only_when_requested() {
        let (_, without) = delegated(Harness::Pi, None);
        assert!(!without.contains(&"--profile".to_owned()));

        let (_, with) = delegated(Harness::Pi, Some(Profile::ReadOnly));
        assert_eq!(
            with[with.len() - 3..],
            strings(&["umbriel-mcp", "--profile", "read-only"])
        );

        let (_, claude) = delegated(Harness::ClaudeCode, Some(Profile::NoExec));
        assert_eq!(
            claude[claude.len() - 3..],
            strings(&["umbriel-mcp", "--profile", "no-exec"])
        );

        let (_, code) = delegated(Harness::Vscode, Some(Profile::NoExec));
        let definition: Value = serde_json::from_str(&code[1]).unwrap();
        assert_eq!(definition["args"], json!(["--profile", "no-exec"]));
    }

    #[test]
    fn an_explicit_full_profile_is_passed_through_too() {
        let (_, arguments) = delegated(Harness::Pi, Some(Profile::Full));
        assert_eq!(
            arguments[arguments.len() - 2..],
            strings(&["--profile", "full"])
        );
    }

    #[test]
    fn opencode_snippet_uses_its_local_server_format() {
        let (file, config) = snippet(Harness::Opencode, None);
        assert!(file.contains("opencode"), "{file}");
        assert_eq!(
            config["mcp"]["umbriel"],
            json!({"type":"local","command":["umbriel-mcp"],"enabled":true})
        );
    }

    #[test]
    fn opencode_command_is_one_array_including_the_profile_flag() {
        let (_, config) = snippet(Harness::Opencode, Some(Profile::NoExec));
        assert_eq!(
            config["mcp"]["umbriel"]["command"],
            json!(["umbriel-mcp", "--profile", "no-exec"])
        );
    }

    #[test]
    fn cursor_snippet_uses_mcp_servers() {
        let (file, config) = snippet(Harness::Cursor, None);
        assert!(file.contains(".cursor/mcp.json"), "{file}");
        assert_eq!(
            config["mcpServers"]["umbriel"],
            json!({"command":"umbriel-mcp","args":[]})
        );
    }

    #[test]
    fn zed_snippet_always_has_an_explicit_empty_argument_list() {
        let (file, config) = snippet(Harness::Zed, None);
        assert!(file.contains("zed"), "{file}");
        let server = &config["context_servers"]["umbriel"];
        assert_eq!(server["command"], json!("umbriel-mcp"));
        assert_eq!(server["args"], json!([]));
    }

    #[test]
    fn zed_snippet_carries_the_profile_in_args() {
        let (_, config) = snippet(Harness::Zed, Some(Profile::ReadOnly));
        assert_eq!(
            config["context_servers"]["umbriel"]["args"],
            json!(["--profile", "read-only"])
        );
    }

    #[test]
    fn a_custom_server_command_path_is_used_everywhere() {
        let Registration::Snippet { text, .. } =
            plan(Harness::Cursor, None, "/opt/tools/umbriel-mcp")
        else {
            panic!("cursor prints a snippet");
        };
        assert!(text.contains("/opt/tools/umbriel-mcp"));
    }

    #[test]
    fn every_registration_tells_the_user_what_to_do_next() {
        for harness in [
            Harness::Pi,
            Harness::ClaudeCode,
            Harness::Vscode,
            Harness::Opencode,
            Harness::Cursor,
            Harness::Zed,
        ] {
            let follow_up = match plan(harness, None, "umbriel-mcp") {
                Registration::Delegate { follow_up, .. }
                | Registration::Snippet { follow_up, .. } => follow_up,
            };
            assert!(follow_up.len() > 10, "{harness:?} needs a follow-up step");
        }
    }
}
