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
        ["mcp", "add", "umbriel", "--exposure", "direct", "--"]
    );
    assert!(arguments[6].ends_with("umbriel-mcp"), "{arguments:?}");
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
fn snippet_harnesses_print_config_and_touch_nothing() {
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
