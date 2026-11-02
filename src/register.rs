use crate::profile::Profile;
use anyhow::{Result, anyhow};
use clap::ValueEnum;
use serde_json::json;
use std::process::{Command, ExitCode};

const SERVER_NAME: &str = "umbriel";
const SERVER_BINARY: &str = "umbriel-mcp";
pub const SERVER_SUMMARY: &str = "Inspect and control this desktop's windows, workspaces, monitors, input, screen and clipboard (Umbriel Wayland compositor).";

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
pub struct Snippet {
    pub file: &'static str,
    pub text: String,
    pub follow_up: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Capability {
    pub help_arguments: &'static [&'static str],
    pub subcommand: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Registration {
    Delegate {
        program: &'static str,
        arguments: Vec<String>,
        follow_up: &'static str,
        capability: Option<Capability>,
        fallback: Option<Snippet>,
        remove_existing: Option<Vec<String>>,
    },
    Snippet(Snippet),
}

impl Registration {
    #[cfg(test)]
    fn delegated_arguments(self) -> (&'static str, Vec<String>) {
        match self {
            Registration::Delegate {
                program, arguments, ..
            } => (program, arguments),
            Registration::Snippet(_) => panic!("expected a delegated registration"),
        }
    }
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
            arguments: words(&[
                "mcp",
                "add",
                SERVER_NAME,
                "--exposure",
                "direct",
                "--description",
                SERVER_SUMMARY,
                "--",
            ]),
            follow_up: "Start a new pi session; the tools are exposed directly to the model.",
            capability: None,
            fallback: None,
            remove_existing: None,
        },
        Harness::ClaudeCode => Registration::Delegate {
            program: "claude",
            arguments: words(&["mcp", "add", "--scope", "user", SERVER_NAME, "--"]),
            follow_up: "Start a new Claude Code session and check `/mcp` lists umbriel.",
            capability: None,
            fallback: None,
            remove_existing: Some(
                ["mcp", "remove", "--scope", "user", SERVER_NAME]
                    .map(str::to_owned)
                    .to_vec(),
            ),
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
            capability: None,
            fallback: None,
            remove_existing: None,
        },
        Harness::Opencode => Registration::Delegate {
            program: "opencode",
            arguments: words(&["mcp", "add", SERVER_NAME, "--"]),
            follow_up: "Restart opencode; `opencode mcp list` should show umbriel.",
            capability: None,
            fallback: Some(Snippet {
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
            }),
            remove_existing: None,
        },
        Harness::Cursor => Registration::Delegate {
            program: "cursor-agent",
            arguments: words(&["mcp", "add", SERVER_NAME, "--"]),
            follow_up: "Restart Cursor and enable umbriel under Cursor Settings > MCP if it is not already on.",
            capability: Some(Capability {
                help_arguments: &["mcp", "--help"],
                subcommand: "add",
            }),
            fallback: Some(Snippet {
                file: "~/.cursor/mcp.json (all projects) or .cursor/mcp.json (one project)",
                text: pretty(json!({
                    "mcpServers": { SERVER_NAME: {
                        "command": server_command,
                        "args": server_arguments(profile),
                    }},
                })),
                follow_up: "Merge the \"umbriel\" entry into \"mcpServers\", then enable it under Cursor Settings > MCP.",
            }),
            remove_existing: None,
        },
        Harness::Zed => Registration::Snippet(Snippet {
            file: "Zed settings.json (open it with the `zed: open settings` command)",
            text: pretty(json!({
                "context_servers": { SERVER_NAME: {
                    "command": server_command,
                    "args": server_arguments(profile),
                }},
            })),
            follow_up: "Merge the \"umbriel\" entry into \"context_servers\"; keep \"args\" even when empty or Zed rejects the whole file.",
        }),
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

fn print_snippet(snippet: &Snippet, reason: Option<String>) {
    if let Some(reason) = reason {
        println!("{reason}\n");
    }
    println!(
        "Add this to {}:\n\n{}\n\n{}",
        snippet.file, snippet.text, snippet.follow_up
    );
    println!("umbriel-mcp does not edit this file for you.");
}

pub fn help_lists_subcommand(help: &str, subcommand: &str) -> bool {
    help.lines().any(|line| {
        line.trim_start()
            .strip_prefix(subcommand)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    })
}

fn supports(program: &str, capability: &Capability) -> bool {
    Command::new(program)
        .args(capability.help_arguments)
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| {
            let help =
                String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
            help_lists_subcommand(&help, capability.subcommand)
        })
        .unwrap_or(false)
}

fn execute(registration: Registration) -> Result<()> {
    let (program, arguments, follow_up, capability, fallback, remove_existing) = match registration
    {
        Registration::Snippet(snippet) => {
            print_snippet(&snippet, None);
            return Ok(());
        }
        Registration::Delegate {
            program,
            arguments,
            follow_up,
            capability,
            fallback,
            remove_existing,
        } => (
            program,
            arguments,
            follow_up,
            capability,
            fallback,
            remove_existing,
        ),
    };
    if !is_on_path(program) {
        return match fallback {
            Some(snippet) => {
                print_snippet(
                    &snippet,
                    Some(format!(
                        "`{program}` was not found on PATH, so it could not register umbriel."
                    )),
                );
                Ok(())
            }
            None => Err(anyhow!(
                "`{program}` was not found on PATH, so nothing was registered. \
                 Install it, or run this yourself later:\n  {}",
                manual_command(program, &arguments)
            )),
        };
    }
    if let (Some(capability), Some(snippet)) = (&capability, &fallback)
        && !supports(program, capability)
    {
        print_snippet(
            snippet,
            Some(format!(
                "This `{program}` has no `{} {}` command, so it could not register umbriel.",
                capability.help_arguments[0], capability.subcommand
            )),
        );
        return Ok(());
    }
    if let Some(removal) = &remove_existing {
        let _ = Command::new(program)
            .args(removal)
            .stdin(std::process::Stdio::null())
            .output();
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
    use super::{Harness, Registration, SERVER_SUMMARY, Snippet, help_lists_subcommand, plan};
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
        let snippet = match plan(harness, profile, "umbriel-mcp") {
            Registration::Snippet(snippet)
            | Registration::Delegate {
                fallback: Some(snippet),
                ..
            } => snippet,
            other => panic!("{harness:?} has no snippet, got {other:?}"),
        };
        let Snippet { file, text, .. } = snippet;
        (
            file,
            serde_json::from_str(&text).expect("snippet is valid JSON"),
        )
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
                "--description",
                SERVER_SUMMARY,
                "--",
                "umbriel-mcp"
            ])
        );
        assert!(SERVER_SUMMARY.contains("this desktop"));
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
        let Registration::Snippet(Snippet { text, .. }) =
            plan(Harness::Zed, None, "/opt/tools/umbriel-mcp")
        else {
            panic!("zed prints a snippet");
        };
        assert!(text.contains("/opt/tools/umbriel-mcp"));
        let (_, arguments) = delegated(Harness::Opencode, None);
        assert_eq!(arguments.last().map(String::as_str), Some("umbriel-mcp"));
        let (_, arguments) =
            super::plan(Harness::Opencode, None, "/opt/tools/umbriel-mcp").delegated_arguments();
        assert_eq!(
            arguments.last().map(String::as_str),
            Some("/opt/tools/umbriel-mcp")
        );
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
                | Registration::Snippet(Snippet { follow_up, .. }) => follow_up,
            };
            assert!(follow_up.len() > 10, "{harness:?} needs a follow-up step");
        }
    }

    #[test]
    fn opencode_is_registered_through_its_own_cli() {
        let (program, arguments) = delegated(Harness::Opencode, None);
        assert_eq!(program, "opencode");
        assert_eq!(
            arguments,
            strings(&["mcp", "add", "umbriel", "--", "umbriel-mcp"])
        );

        let (_, with_profile) = delegated(Harness::Opencode, Some(Profile::NoExec));
        assert_eq!(
            with_profile,
            strings(&[
                "mcp",
                "add",
                "umbriel",
                "--",
                "umbriel-mcp",
                "--profile",
                "no-exec"
            ])
        );
    }

    #[test]
    fn cursor_is_registered_through_cursor_agent_only_when_it_offers_mcp_add() {
        let registration = plan(Harness::Cursor, None, "umbriel-mcp");
        let Registration::Delegate {
            program,
            arguments,
            capability: Some(capability),
            fallback: Some(_),
            ..
        } = registration
        else {
            panic!("cursor should delegate with a capability check and a fallback");
        };
        assert_eq!(program, "cursor-agent");
        assert_eq!(
            arguments,
            strings(&["mcp", "add", "umbriel", "--", "umbriel-mcp"])
        );
        assert_eq!(capability.help_arguments, ["mcp", "--help"]);
        assert_eq!(capability.subcommand, "add");
    }

    #[test]
    fn harnesses_that_are_their_own_cli_have_no_snippet_fallback() {
        for harness in [Harness::Pi, Harness::ClaudeCode, Harness::Vscode] {
            assert!(matches!(
                plan(harness, None, "umbriel-mcp"),
                Registration::Delegate { fallback: None, .. }
            ));
        }
    }

    #[test]
    fn a_help_listing_is_read_for_real_subcommands_only() {
        let help =
            "Commands:\n  list       List servers\n  add <name>  Add a server\n  addons  x\n";
        assert!(help_lists_subcommand(help, "add"));
        assert!(help_lists_subcommand("  add\n", "add"));
        assert!(!help_lists_subcommand(
            "Commands:\n  list\n  addons\n",
            "add"
        ));
        assert!(!help_lists_subcommand("Use this to add things\n", "add"));
    }

    #[test]
    fn claude_code_replaces_an_existing_registration_instead_of_failing() {
        let Registration::Delegate {
            remove_existing, ..
        } = plan(Harness::ClaudeCode, None, "umbriel-mcp")
        else {
            panic!("claude code delegates");
        };
        assert_eq!(
            remove_existing,
            Some(strings(&["mcp", "remove", "--scope", "user", "umbriel"]))
        );
    }

    #[test]
    fn harnesses_whose_add_already_replaces_skip_the_removal() {
        for harness in [
            Harness::Pi,
            Harness::Opencode,
            Harness::Vscode,
            Harness::Cursor,
        ] {
            assert!(
                matches!(
                    plan(harness, None, "umbriel-mcp"),
                    Registration::Delegate {
                        remove_existing: None,
                        ..
                    }
                ),
                "{harness:?}"
            );
        }
    }
}
