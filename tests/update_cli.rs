use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);
const ARCH: &str = std::env::consts::ARCH;
const SYSTEM_PATH: &str = "/usr/bin:/bin";

fn scratch(purpose: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "umbriel-update-{purpose}-{}-{}",
        std::process::id(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&directory).expect("create scratch directory");
    directory
}

fn fake_binary(version: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'umbriel-mcp {version}' ;;\n  doctor) echo 'fake doctor ran' ;;\nesac\n"
    )
}

struct FakeRelease {
    root: PathBuf,
}

impl FakeRelease {
    fn publish(version: &str, binary_text: &str, tamper_checksum: bool) -> Self {
        let root = scratch("release");
        let tag = format!("v{version}");
        let folder = format!("umbriel-mcp-{tag}-{ARCH}-linux");
        let staging = scratch("staging");
        std::fs::create_dir_all(staging.join(&folder)).unwrap();
        let binary = staging.join(&folder).join("umbriel-mcp");
        std::fs::write(&binary, binary_text).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let download = root.join("download").join(&tag);
        std::fs::create_dir_all(&download).unwrap();
        let asset = format!("{folder}.tar.gz");
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(download.join(&asset))
            .arg("-C")
            .arg(&staging)
            .arg(&folder)
            .status()
            .unwrap();
        assert!(packed.success());
        let summed = Command::new("sha256sum")
            .arg(&asset)
            .current_dir(&download)
            .output()
            .unwrap();
        let mut sums = String::from_utf8(summed.stdout).unwrap();
        if tamper_checksum {
            sums = format!("{}{}", "0".repeat(64), &sums[64..]);
        }
        let latest = root.join("latest").join("download");
        std::fs::create_dir_all(&latest).unwrap();
        std::fs::write(latest.join("SHA256SUMS"), sums).unwrap();
        Self { root }
    }

    fn url(&self) -> String {
        format!("file://{}", self.root.display())
    }
}

fn installed_copy(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let executable = folder.join("umbriel-mcp");
    std::fs::copy(env!("CARGO_BIN_EXE_umbriel-mcp"), &executable).unwrap();
    executable
}

fn run_update(executable: &Path, release: &FakeRelease, arguments: &[&str], path: &str) -> Output {
    Command::new(executable)
        .arg("update")
        .args(arguments)
        .env("UMBRIEL_MCP_RELEASES", release.url())
        .env("PATH", path)
        .output()
        .expect("run update")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn is_original(executable: &Path) -> bool {
    std::fs::read(executable).unwrap() == std::fs::read(env!("CARGO_BIN_EXE_umbriel-mcp")).unwrap()
}

#[test]
fn a_newer_release_replaces_the_binary_in_place() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let executable = installed_copy(&scratch("home").join(".local/bin"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains(&format!("from {} to 99.0.0", env!("CARGO_PKG_VERSION"))));
    assert_eq!(
        std::fs::read_to_string(&executable).unwrap(),
        fake_binary("99.0.0")
    );
    assert_eq!(
        std::fs::metadata(&executable).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let leftovers: Vec<_> = std::fs::read_dir(executable.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
}

#[test]
fn the_current_version_is_left_alone() {
    let version = env!("CARGO_PKG_VERSION");
    let release = FakeRelease::publish(version, &fake_binary(version), false);
    let executable = installed_copy(&scratch("same"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(output.status.success());
    assert!(text(&output).contains("is up to date"), "{}", text(&output));
    assert!(is_original(&executable));
}

#[test]
fn check_only_reports_without_changing_anything() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let executable = installed_copy(&scratch("check"));

    let output = run_update(&executable, &release, &["--check"], SYSTEM_PATH);

    assert!(
        text(&output).contains("Update available: 99.0.0"),
        "{}",
        text(&output)
    );
    assert!(text(&output).contains("run `umbriel-mcp update`"));
    assert!(is_original(&executable));
}

#[test]
fn a_download_that_does_not_match_its_checksum_is_refused() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), true);
    let executable = installed_copy(&scratch("tampered"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(!output.status.success());
    assert!(
        text(&output).contains("does not match its published checksum"),
        "{}",
        text(&output)
    );
    assert!(is_original(&executable));
}

#[test]
fn a_binary_that_reports_the_wrong_version_is_refused() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("1.2.3"), false);
    let executable = installed_copy(&scratch("wrong-version"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(!output.status.success());
    assert!(
        text(&output).contains("instead of \"umbriel-mcp 99.0.0\""),
        "{}",
        text(&output)
    );
    assert!(is_original(&executable));
}

#[test]
fn an_unwritable_install_folder_asks_for_sudo() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let folder = scratch("locked");
    let executable = installed_copy(&folder);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!output.status.success());
    assert!(
        text(&output).contains("sudo umbriel-mcp update"),
        "{}",
        text(&output)
    );
    assert!(is_original(&executable));
}

#[test]
fn a_cargo_installation_is_pointed_at_cargo() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let executable = installed_copy(&scratch("cargo").join(".cargo/bin"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(!output.status.success());
    assert!(
        text(&output).contains("cargo install umbriel-mcp --locked"),
        "{}",
        text(&output)
    );
    assert!(is_original(&executable));
}

#[test]
fn without_curl_the_update_explains_what_is_missing() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let executable = installed_copy(&scratch("no-curl"));

    let output = run_update(
        &executable,
        &release,
        &[],
        scratch("empty-path").to_str().unwrap(),
    );

    assert!(!output.status.success());
    assert!(
        text(&output).contains("updating needs curl"),
        "{}",
        text(&output)
    );
}

fn run_installer(release: &FakeRelease, home: &Path, extra_path: Option<&Path>) -> Output {
    let path = match extra_path {
        Some(extra) => format!("{}:{SYSTEM_PATH}", extra.display()),
        None => SYSTEM_PATH.to_owned(),
    };
    Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
        .env("UMBRIEL_MCP_RELEASES", release.url())
        .env("HOME", home)
        .env_remove("UMBRIEL_MCP_INSTALL_DIR")
        .env("PATH", path)
        .output()
        .expect("run install.sh")
}

#[test]
fn the_installer_puts_the_verified_binary_in_local_bin_and_runs_doctor() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let home = scratch("installer-home");

    let output = run_installer(&release, &home, None);

    assert!(output.status.success(), "{}", text(&output));
    let installed = home.join(".local/bin/umbriel-mcp");
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        fake_binary("99.0.0")
    );
    let report = text(&output);
    assert!(report.contains("Installed umbriel-mcp 99.0.0"), "{report}");
    assert!(report.contains("is not on your PATH yet"), "{report}");
    assert!(report.contains("fake doctor ran"), "{report}");
}

#[test]
fn the_installer_refuses_a_tampered_download() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), true);
    let home = scratch("installer-tampered");

    let output = run_installer(&release, &home, None);

    assert!(!output.status.success());
    assert!(
        text(&output).contains("does not match its published checksum"),
        "{}",
        text(&output)
    );
    assert!(!home.join(".local/bin/umbriel-mcp").exists());
}

#[test]
fn the_installer_warns_about_an_older_copy_earlier_on_the_path() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let home = scratch("installer-shadowed");
    let old_location = scratch("usr-local-bin");
    std::fs::write(old_location.join("umbriel-mcp"), fake_binary("0.1.0")).unwrap();
    std::fs::set_permissions(
        old_location.join("umbriel-mcp"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let output = run_installer(&release, &home, Some(&old_location));

    let report = text(&output);
    assert!(output.status.success(), "{report}");
    assert!(report.contains("comes first on your PATH"), "{report}");
    assert!(
        report.contains(&format!(
            "sudo rm {}",
            old_location.join("umbriel-mcp").display()
        )),
        "{report}"
    );
}
