mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use support::scratch_directory;

const PI_CONFIG: &str = r#"{
  "mcpServers": {
    "github": { "command": "github-mcp" },
    "umbriel": { "command": "umbriel-mcp", "args": [] }
  }
}
"#;

const OPENCODE_CONFIG: &str = r#"{
  // personal opencode settings
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "umbriel": { "type": "local", "command": ["umbriel-mcp"], "enabled": true },
    "penpot": { "type": "remote", "url": "http://localhost:4401" } // design work
  },
}
"#;

const CURSOR_CONFIG: &str = r#"{
  "mcpServers": {
    "umbriel": { "command": "umbriel-mcp", "args": ["--profile", "no-exec"] }
  }
}
"#;

const VSCODE_CONFIG: &str = r#"{
  "servers": {
    "umbriel": { "command": "/home/u/.local/bin/umbriel-mcp", "args": [] },
    "other": { "command": "other-mcp" }
  }
}
"#;

const ZED_SETTINGS: &str = r#"// Zed settings
{
  "theme": "One Dark",
  "context_servers": {
    // desktop control
    "umbriel": { "command": "umbriel-mcp", "args": [] }
  }
}
"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn home_registered_everywhere_by_file() -> PathBuf {
    let home = scratch_directory("uninstall-home");
    write(&home.join(".pi/agent/mcp.json"), PI_CONFIG);
    write(
        &home.join(".config/opencode/opencode.jsonc"),
        OPENCODE_CONFIG,
    );
    write(&home.join(".cursor/mcp.json"), CURSOR_CONFIG);
    write(&home.join(".config/Code/User/mcp.json"), VSCODE_CONFIG);
    write(&home.join(".config/zed/settings.json"), ZED_SETTINGS);
    home
}

fn uninstall_with(executable: &Path, arguments: &[&str], home: &Path, programs: &Path) -> Output {
    Command::new(executable)
        .arg("uninstall")
        .args(arguments)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", format!("{}:/usr/bin:/bin", programs.display()))
        .stdin(Stdio::null())
        .output()
        .expect("run uninstall")
}

fn uninstall(arguments: &[&str], home: &Path, programs: &Path) -> Output {
    uninstall_with(
        Path::new(env!("CARGO_BIN_EXE_umbriel-mcp")),
        arguments,
        home,
        programs,
    )
}

fn stdout(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn fake_claude(programs: &Path, registered_command: &str) -> PathBuf {
    let record = programs.join("claude.args");
    let state = programs.join("claude.registered");
    std::fs::write(&state, registered_command).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{record}'\ncase \"$*\" in\n  'mcp get umbriel') [ -s '{state}' ] || exit 1; printf 'umbriel:\\n  Scope: User config\\n  Command: %s\\n' \"$(cat '{state}')\";;\n  'mcp remove --scope user umbriel') : > '{state}'; echo 'Removed MCP server umbriel';;\nesac\n",
        record = record.display(),
        state = state.display(),
    );
    let path = programs.join("claude");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    record
}

#[test]
fn file_registrations_are_removed_keeping_comments_and_other_servers() {
    let home = home_registered_everywhere_by_file();
    let programs = scratch_directory("uninstall-programs");

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    assert!(output.status.success(), "{}", stdout(&output));
    let opencode = read(&home.join(".config/opencode/opencode.jsonc"));
    assert!(!opencode.contains("umbriel"), "{opencode}");
    assert!(
        opencode.contains("// personal opencode settings"),
        "{opencode}"
    );
    assert!(opencode.contains("\"penpot\""), "{opencode}");
    assert!(opencode.contains("// design work"), "{opencode}");
    let pi = read(&home.join(".pi/agent/mcp.json"));
    assert!(!pi.contains("umbriel") && pi.contains("github-mcp"), "{pi}");
    assert!(!read(&home.join(".cursor/mcp.json")).contains("umbriel"));
    let vscode = read(&home.join(".config/Code/User/mcp.json"));
    assert!(
        !vscode.contains("umbriel") && vscode.contains("other-mcp"),
        "{vscode}"
    );
    let zed = read(&home.join(".config/zed/settings.json"));
    assert!(!zed.contains("umbriel"), "{zed}");
    assert!(
        zed.contains("// Zed settings") && zed.contains("One Dark"),
        "{zed}"
    );
    let report = stdout(&output);
    for harness in ["pi", "opencode", "Cursor", "VS Code", "Zed"] {
        assert!(report.contains(harness), "missing {harness}: {report}");
    }
}

#[test]
fn claude_code_is_unregistered_through_its_own_cli() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    let record = fake_claude(&programs, "umbriel-mcp");

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    assert!(output.status.success(), "{}", stdout(&output));
    let calls = read(&record);
    assert!(calls.contains("mcp remove --scope user umbriel"), "{calls}");
    assert!(
        stdout(&output).contains("Claude Code"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_server_named_umbriel_that_is_not_umbriel_mcp_is_left_alone() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    let foreign = PI_CONFIG.replace("\"umbriel-mcp\"", "\"someone-elses-server\"");
    write(&home.join(".pi/agent/mcp.json"), &foreign);
    let record = fake_claude(&programs, "someone-elses-server");

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    assert_eq!(read(&home.join(".pi/agent/mcp.json")), foreign);
    assert!(!read(&record).contains("remove"), "{}", read(&record));
    assert!(
        stdout(&output).contains("not registered"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn without_yes_and_without_a_terminal_it_only_shows_the_plan() {
    let home = home_registered_everywhere_by_file();
    let programs = scratch_directory("uninstall-programs");

    let output = uninstall(&["--keep-binary"], &home, &programs);

    assert!(!output.status.success());
    assert_eq!(read(&home.join(".pi/agent/mcp.json")), PI_CONFIG);
    let report = stdout(&output);
    assert!(report.contains(".pi/agent/mcp.json"), "{report}");
    assert!(report.contains("--yes"), "{report}");
    assert!(report.contains("Nothing was changed"), "{report}");
}

#[test]
fn with_nothing_registered_it_says_so() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("not registered with any harness"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_config_it_cannot_read_is_reported_for_removal_by_hand() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    let broken = "{ \"context_servers\": { \"umbriel\": { \"command\": \"umbriel-mcp\" ";
    write(&home.join(".config/zed/settings.json"), broken);
    write(&home.join(".pi/agent/mcp.json"), PI_CONFIG);

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    assert!(!output.status.success());
    assert_eq!(read(&home.join(".config/zed/settings.json")), broken);
    assert!(!read(&home.join(".pi/agent/mcp.json")).contains("umbriel"));
    let report = stdout(&output);
    assert!(report.contains("by hand"), "{report}");
    assert!(report.contains("zed/settings.json"), "{report}");
}

fn installed_copy(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let executable = folder.join("umbriel-mcp");
    std::fs::copy(env!("CARGO_BIN_EXE_umbriel-mcp"), &executable).unwrap();
    executable
}

fn run_copy(executable: &Path, home: &Path, programs: &Path) -> Output {
    for _ in 0..100 {
        match Command::new(executable)
            .args(["uninstall", "--yes"])
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("PATH", format!("{}:/usr/bin:/bin", programs.display()))
            .stdin(Stdio::null())
            .output()
        {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other.expect("run uninstall"),
        }
    }
    panic!("{} stayed busy", executable.display())
}

#[test]
fn a_binary_installed_by_the_script_removes_itself() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    let executable = installed_copy(&home.join(".local/bin"));

    let output = run_copy(&executable, &home, &programs);

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(!executable.exists());
    assert!(stdout(&output).contains("Removed"), "{}", stdout(&output));
}

#[test]
fn run_through_a_symlink_only_the_link_is_removed() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    let real = installed_copy(&scratch_directory("uninstall-build"));
    let link = home.join(".local/bin/umbriel-mcp");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let output = run_copy(&link, &home, &programs);

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(real.exists());
}

#[test]
fn backups_and_recordings_are_listed_but_kept() {
    let home = scratch_directory("uninstall-home");
    let programs = scratch_directory("uninstall-programs");
    write(&home.join(".config/umbriel/config.toml"), "");
    write(&home.join(".config/umbriel/config.toml.bak-1793772995"), "");
    write(
        &home.join(".config/umbriel/config.toml.bak-1793772995-2"),
        "",
    );
    write(&home.join("Videos/umbriel-mcp/recording.mp4"), "");

    let output = uninstall(&["--yes", "--keep-binary"], &home, &programs);

    let report = stdout(&output);
    assert!(report.contains("2 config backups"), "{report}");
    assert!(report.contains("Videos/umbriel-mcp"), "{report}");
    assert!(
        home.join(".config/umbriel/config.toml.bak-1793772995")
            .exists()
    );
    assert!(home.join("Videos/umbriel-mcp/recording.mp4").exists());
}
