use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch_directory(purpose: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "umbriel-install-{purpose}-{}-{}",
        std::process::id(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&directory).expect("create scratch directory");
    directory
}

fn install_recording_program(directory: &Path, name: &str, exit_code: i32) -> PathBuf {
    let record = directory.join(format!("{name}.args"));
    let path = directory.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\n' \"$argument\" >> '{}'; done\nexit {exit_code}\n",
            record.display()
        ),
    )
    .expect("write fake harness");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    record
}

fn run_install(arguments: &[&str], programs: &Path, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
        .arg("install")
        .args(arguments)
        .env("PATH", programs)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("run install")
}

fn recorded(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn directory_is_empty(directory: &Path) -> bool {
    std::fs::read_dir(directory)
        .expect("read home")
        .next()
        .is_none()
}

#[test]
fn pi_registration_runs_the_pi_cli_with_direct_exposure() {
    let programs = scratch_directory("pi-bin");
    let home = scratch_directory("pi-home");
    let record = install_recording_program(&programs, "pi", 0);

    let output = run_install(&["pi"], &programs, &home);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = recorded(&record);
    assert_eq!(
        &arguments[..6],
        [
            "mcp",
            "add",
            "umbriel",
            "--exposure",
            "direct",
            "--description"
        ]
    );
    assert!(arguments[6].contains("this desktop"), "{arguments:?}");
    assert_eq!(arguments[7], "--");
    assert!(arguments[8].ends_with("umbriel-mcp"), "{arguments:?}");
    assert!(!arguments.contains(&"--profile".to_owned()));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Registered umbriel"));
    assert!(
        directory_is_empty(&home),
        "install must not write into HOME"
    );
}

#[test]
fn an_explicit_profile_reaches_the_harness_command() {
    let programs = scratch_directory("profile-bin");
    let home = scratch_directory("profile-home");
    let record = install_recording_program(&programs, "claude", 0);

    let output = run_install(&["claude-code", "--profile", "read-only"], &programs, &home);

    assert!(output.status.success());
    let arguments = recorded(&record);
    assert_eq!(
        &arguments[arguments.len() - 2..],
        ["--profile", "read-only"]
    );
}

#[test]
fn a_missing_harness_cli_fails_without_claiming_success() {
    let programs = scratch_directory("empty-bin");
    let home = scratch_directory("missing-home");

    let output = run_install(&["pi"], &programs, &home);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains("Registered"), "{stdout}");
    assert!(stderr.contains("`pi` was not found"), "{stderr}");
    assert!(stderr.contains("nothing was registered"), "{stderr}");
    assert!(
        stderr.contains("'mcp' 'add'"),
        "should show the manual command: {stderr}"
    );
}

#[test]
fn a_harness_cli_that_fails_is_not_reported_as_registered() {
    let programs = scratch_directory("failing-bin");
    let home = scratch_directory("failing-home");
    install_recording_program(&programs, "code", 3);

    let output = run_install(&["vscode"], &programs, &home);

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Registered"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("registration was not completed"));
}

#[test]
fn without_their_cli_snippet_harnesses_print_config_and_touch_nothing() {
    for (harness, expected) in [
        ("opencode", "\"mcp\""),
        ("cursor", "\"mcpServers\""),
        ("zed", "\"context_servers\""),
    ] {
        let programs = scratch_directory("snippet-bin");
        let home = scratch_directory("snippet-home");

        let output = run_install(&[harness], &programs, &home);

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{harness}");
        assert!(stdout.contains(expected), "{harness}: {stdout}");
        assert!(
            stdout.contains("does not edit this file"),
            "{harness}: {stdout}"
        );
        assert!(
            directory_is_empty(&home),
            "{harness} must not write into HOME"
        );
    }
}

#[test]
fn zed_output_keeps_the_empty_args_list() {
    let programs = scratch_directory("zed-bin");
    let home = scratch_directory("zed-home");

    let stdout =
        String::from_utf8_lossy(&run_install(&["zed"], &programs, &home).stdout).into_owned();

    assert!(stdout.contains("\"args\": []"), "{stdout}");
}

#[test]
fn an_unsupported_harness_lists_the_supported_ones() {
    let programs = scratch_directory("unsupported-bin");
    let home = scratch_directory("unsupported-home");

    let output = run_install(&["emacs"], &programs, &home);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    for supported in ["pi", "claude-code", "vscode", "opencode", "cursor", "zed"] {
        assert!(
            stderr.contains(supported),
            "should list {supported}: {stderr}"
        );
    }
}

fn install_program_with_help(directory: &Path, name: &str, help: &str) -> PathBuf {
    let record = directory.join(format!("{name}.args"));
    let path = directory.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$2\" = \"--help\" ]; then printf '%s\\n' '{help}'; fi\nexit 0\n",
            record.display()
        ),
    )
    .expect("write fake harness");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    record
}

fn recorded_invocations(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn opencode_registration_runs_opencode_mcp_add() {
    let programs = scratch_directory("opencode-bin");
    let home = scratch_directory("opencode-home");
    let record = install_recording_program(&programs, "opencode", 0);

    let output = run_install(&["opencode", "--profile", "no-exec"], &programs, &home);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = recorded(&record);
    assert_eq!(&arguments[..4], ["mcp", "add", "umbriel", "--"]);
    assert!(arguments[4].ends_with("umbriel-mcp"), "{arguments:?}");
    assert_eq!(&arguments[5..], ["--profile", "no-exec"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Registered umbriel with opencode"),
        "{stdout}"
    );
    assert!(!stdout.contains("Add this to"), "{stdout}");
    assert!(
        directory_is_empty(&home),
        "install must not write into HOME"
    );
}

#[test]
fn a_failing_opencode_is_not_reported_as_registered() {
    let programs = scratch_directory("opencode-fail-bin");
    let home = scratch_directory("opencode-fail-home");
    install_recording_program(&programs, "opencode", 1);

    let output = run_install(&["opencode"], &programs, &home);

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Registered"));
}

#[test]
fn without_opencode_the_snippet_explains_why_it_is_shown() {
    let programs = scratch_directory("opencode-missing-bin");
    let home = scratch_directory("opencode-missing-home");

    let output = run_install(&["opencode"], &programs, &home);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("`opencode` was not found on PATH"),
        "{stdout}"
    );
    assert!(stdout.contains("\"type\": \"local\""), "{stdout}");
}

#[test]
fn a_cursor_agent_that_offers_mcp_add_registers_umbriel() {
    let programs = scratch_directory("cursor-bin");
    let home = scratch_directory("cursor-home");
    let record = install_program_with_help(
        &programs,
        "cursor-agent",
        "Commands:\n  list   List servers\n  add <name>   Add a server",
    );

    let output = run_install(&["cursor"], &programs, &home);

    let invocations = recorded_invocations(&record);
    assert_eq!(invocations[0], "mcp --help");
    assert!(
        invocations[1].starts_with("mcp add umbriel -- "),
        "{invocations:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Registered umbriel with cursor-agent")
    );
    assert!(directory_is_empty(&home));
}

#[test]
fn a_cursor_agent_without_mcp_add_falls_back_to_the_snippet() {
    let programs = scratch_directory("cursor-old-bin");
    let home = scratch_directory("cursor-old-home");
    let record = install_program_with_help(
        &programs,
        "cursor-agent",
        "Commands:\n  list   List servers\n  enable <id>   Approve a server",
    );

    let output = run_install(&["cursor"], &programs, &home);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("has no `mcp add` command"), "{stdout}");
    assert!(stdout.contains("\"mcpServers\""), "{stdout}");
    assert_eq!(
        recorded_invocations(&record),
        ["mcp --help"],
        "only the help may run"
    );
    assert!(directory_is_empty(&home));
}

#[test]
fn reinstalling_claude_code_removes_the_old_entry_quietly_then_adds() {
    let programs = scratch_directory("claude-again-bin");
    let home = scratch_directory("claude-again-home");
    let record = install_program_with_help(&programs, "claude", "");

    let output = run_install(&["claude-code", "--profile", "read-only"], &programs, &home);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = recorded_invocations(&record);
    assert_eq!(invocations[0], "mcp remove --scope user umbriel");
    assert!(
        invocations[1].starts_with("mcp add --scope user umbriel -- "),
        "{invocations:?}"
    );
    assert!(
        invocations[1].ends_with("--profile read-only"),
        "{invocations:?}"
    );
}
