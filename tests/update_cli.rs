mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::scratch_directory;

const ARCH: &str = std::env::consts::ARCH;
const SYSTEM_PATH: &str = "/usr/bin:/bin";

fn fake_binary(version: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'umbriel-mcp {version}' ;;\n  doctor) echo 'fake doctor ran' ;;\nesac\n"
    )
}

struct FakeRelease {
    root: PathBuf,
    public_key: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signing {
    Correct,
    Missing,
    WrongKey,
    SumsEditedAfterwards,
}

fn openssl(arguments: &[&std::ffi::OsStr]) {
    let status = Command::new("openssl")
        .args(arguments)
        .status()
        .expect("run openssl");
    assert!(status.success(), "openssl {arguments:?} failed");
}

fn new_key_pair(folder: &Path, name: &str) -> (PathBuf, PathBuf) {
    let private = folder.join(format!("{name}-private.pem"));
    let public = folder.join(format!("{name}-public.pem"));
    openssl(&[
        "genpkey".as_ref(),
        "-algorithm".as_ref(),
        "ed25519".as_ref(),
        "-out".as_ref(),
        private.as_os_str(),
    ]);
    openssl(&[
        "pkey".as_ref(),
        "-in".as_ref(),
        private.as_os_str(),
        "-pubout".as_ref(),
        "-out".as_ref(),
        public.as_os_str(),
    ]);
    (private, public)
}

fn sign(private: &Path, message: &Path, signature: &Path) {
    openssl(&[
        "pkeyutl".as_ref(),
        "-sign".as_ref(),
        "-inkey".as_ref(),
        private.as_os_str(),
        "-rawin".as_ref(),
        "-in".as_ref(),
        message.as_os_str(),
        "-out".as_ref(),
        signature.as_os_str(),
    ]);
}

impl FakeRelease {
    fn publish(version: &str, binary_text: &str, tamper_checksum: bool) -> Self {
        Self::publish_signed(version, binary_text, tamper_checksum, Signing::Correct)
    }

    fn publish_signed(
        version: &str,
        binary_text: &str,
        tamper_checksum: bool,
        signing: Signing,
    ) -> Self {
        let keys = scratch_directory("keys");
        let (trusted_private, trusted_public) = new_key_pair(&keys, "trusted");
        let root = scratch_directory("release");
        let tag = format!("v{version}");
        let folder = format!("umbriel-mcp-{tag}-{ARCH}-linux");
        let staging = scratch_directory("staging");
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
        std::fs::write(latest.join("SHA256SUMS"), &sums).unwrap();
        let signature = latest.join("SHA256SUMS.sig");
        match signing {
            Signing::Missing => {}
            Signing::Correct | Signing::SumsEditedAfterwards => {
                sign(&trusted_private, &latest.join("SHA256SUMS"), &signature)
            }
            Signing::WrongKey => {
                let (other_private, _) = new_key_pair(&keys, "other");
                sign(&other_private, &latest.join("SHA256SUMS"), &signature);
            }
        }
        if signing == Signing::SumsEditedAfterwards {
            std::fs::write(
                latest.join("SHA256SUMS"),
                format!("{sums}{}  extra-file\n", "1".repeat(64)),
            )
            .unwrap();
        }
        Self {
            root,
            public_key: trusted_public,
        }
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

struct WritableOnDrop(PathBuf);

impl Drop for WritableOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

fn run_update(executable: &Path, release: &FakeRelease, arguments: &[&str], path: &str) -> Output {
    for _ in 0..100 {
        let attempt = Command::new(executable)
            .arg("update")
            .args(arguments)
            .env("UMBRIEL_MCP_RELEASES", release.url())
            .env("UMBRIEL_MCP_RELEASE_KEY", &release.public_key)
            .env("PATH", path)
            .output();
        match attempt {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other.expect("run update"),
        }
    }
    panic!(
        "{} stayed busy: another test thread kept it open for writing",
        executable.display()
    )
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
    let executable = installed_copy(&scratch_directory("home").join(".local/bin"));

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
    let executable = installed_copy(&scratch_directory("same"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(output.status.success());
    assert!(text(&output).contains("is up to date"), "{}", text(&output));
    assert!(is_original(&executable));
}

#[test]
fn check_only_reports_without_changing_anything() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let executable = installed_copy(&scratch_directory("check"));

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
    let executable = installed_copy(&scratch_directory("tampered"));

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
    let executable = installed_copy(&scratch_directory("wrong-version"));

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
    let folder = scratch_directory("locked");
    let executable = installed_copy(&folder);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _writable_again = WritableOnDrop(folder.clone());

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

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
    let executable = installed_copy(&scratch_directory("cargo").join(".cargo/bin"));

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
    let executable = installed_copy(&scratch_directory("no-curl"));

    let output = run_update(
        &executable,
        &release,
        &[],
        scratch_directory("empty-path").to_str().unwrap(),
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
        .env("UMBRIEL_MCP_RELEASE_KEY", &release.public_key)
        .env("HOME", home)
        .env_remove("UMBRIEL_MCP_INSTALL_DIR")
        .env("PATH", path)
        .output()
        .expect("run install.sh")
}

#[test]
fn the_installer_puts_the_verified_binary_in_local_bin_and_runs_doctor() {
    let release = FakeRelease::publish("99.0.0", &fake_binary("99.0.0"), false);
    let home = scratch_directory("installer-home");

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
    let home = scratch_directory("installer-tampered");

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
    let home = scratch_directory("installer-shadowed");
    let old_location = scratch_directory("usr-local-bin");
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

fn assert_update_refused(signing: Signing, expected: &str) {
    let release = FakeRelease::publish_signed("99.0.0", &fake_binary("99.0.0"), false, signing);
    let executable = installed_copy(&scratch_directory("signing"));

    let output = run_update(&executable, &release, &[], SYSTEM_PATH);

    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains(expected), "{}", text(&output));
    assert!(is_original(&executable));
}

#[test]
fn an_unsigned_release_is_not_installed_by_update() {
    assert_update_refused(Signing::Missing, "is not signed");
}

#[test]
fn a_release_signed_with_another_key_is_not_installed_by_update() {
    assert_update_refused(Signing::WrongKey, "signature does not match");
}

#[test]
fn a_checksum_list_changed_after_signing_is_not_installed_by_update() {
    assert_update_refused(Signing::SumsEditedAfterwards, "signature does not match");
}

fn assert_installer_refused(signing: Signing, expected: &str) {
    let release = FakeRelease::publish_signed("99.0.0", &fake_binary("99.0.0"), false, signing);
    let home = scratch_directory("installer-signing");

    let output = run_installer(&release, &home, None);

    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains(expected), "{}", text(&output));
    assert!(!home.join(".local/bin/umbriel-mcp").exists());
}

#[test]
fn the_installer_refuses_unsigned_and_wrongly_signed_releases() {
    assert_installer_refused(Signing::Missing, "is not signed");
    assert_installer_refused(Signing::WrongKey, "signature does not match");
    assert_installer_refused(Signing::SumsEditedAfterwards, "signature does not match");
}
