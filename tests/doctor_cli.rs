use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch_directory(purpose: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "umbriel-doctor-{purpose}-{}-{}",
        std::process::id(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&directory).expect("create scratch directory");
    directory
}

fn install_fake_program(directory: &Path, name: &str, script_body: &str) {
    let path = directory.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).expect("write fake program");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make fake program executable");
}

fn tesseract_listing(languages: &[&str]) -> String {
    let mut lines = vec![format!(
        "List of available languages in \"/fake/tessdata/\" ({}):",
        languages.len()
    )];
    lines.extend(languages.iter().map(|language| (*language).to_owned()));
    format!(
        "printf '%s\\n' {}",
        lines
            .iter()
            .map(|line| format!("'{line}'"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

struct ListeningSocket {
    path: PathBuf,
}

impl ListeningSocket {
    fn start() -> Self {
        let path = scratch_directory("socket").join("umbriel.sock");
        let listener = UnixListener::bind(&path).expect("bind fake Umbriel socket");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let mut writer = stream;
                    let _ = writeln!(writer, "{{\"ok\":[]}}");
                }
            }
        });
        Self { path }
    }
}

fn run_doctor(socket: &Path, programs: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
        .arg("doctor")
        .env("UMBRIEL_SOCKET", socket)
        .env("PATH", programs)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .expect("run doctor")
}

fn report(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn an_unreachable_compositor_and_no_helpers_fail_every_check_individually() {
    let nothing_installed = scratch_directory("empty");

    let output = run_doctor(Path::new("/nonexistent/umbriel.sock"), &nothing_installed);
    let report = report(&output);

    assert!(!output.status.success(), "{report}");
    for failing in [
        "[fail] Umbriel socket",
        "[fail] Virtual pointer protocol",
        "[fail] Virtual keyboard protocol",
        "[fail] Screen capture (grim)",
        "[fail] OCR (tesseract)",
        "[fail] OCR English data",
    ] {
        assert!(
            report.contains(failing),
            "missing {failing:?} in:\n{report}"
        );
    }
    assert!(report.contains("6 of 6 checks failed"), "{report}");
}

#[test]
fn present_helpers_and_a_reachable_socket_pass_their_own_checks() {
    let programs = scratch_directory("complete");
    install_fake_program(&programs, "grim", "exit 0");
    install_fake_program(&programs, "tesseract", &tesseract_listing(&["eng", "osd"]));
    let socket = ListeningSocket::start();

    let output = run_doctor(&socket.path, &programs);
    let report = report(&output);

    for ready in [
        "[ok]   Umbriel socket",
        "[ok]   Screen capture (grim)",
        "[ok]   OCR (tesseract)",
        "[ok]   OCR English data",
    ] {
        assert!(report.contains(ready), "missing {ready:?} in:\n{report}");
    }
    assert!(
        !output.status.success(),
        "protocol checks cannot pass without a Wayland session:\n{report}"
    );
    assert!(report.contains("2 of 6 checks failed"), "{report}");
}

#[test]
fn tesseract_without_english_data_is_reported_separately_from_the_program() {
    let programs = scratch_directory("no-english");
    install_fake_program(&programs, "grim", "exit 0");
    install_fake_program(&programs, "tesseract", &tesseract_listing(&["osd", "afr"]));
    let socket = ListeningSocket::start();

    let output = run_doctor(&socket.path, &programs);
    let report = report(&output);

    assert!(report.contains("[ok]   OCR (tesseract)"), "{report}");
    assert!(report.contains("[fail] OCR English data"), "{report}");
    assert!(report.contains("3 of 6 checks failed"), "{report}");
}

#[test]
fn a_tesseract_that_cannot_list_languages_fails_the_data_check() {
    let programs = scratch_directory("broken-tesseract");
    install_fake_program(&programs, "grim", "exit 0");
    install_fake_program(&programs, "tesseract", "echo broken >&2; exit 1");
    let socket = ListeningSocket::start();

    let output = run_doctor(&socket.path, &programs);
    let report = report(&output);

    assert!(report.contains("[ok]   OCR (tesseract)"), "{report}");
    assert!(report.contains("[fail] OCR English data"), "{report}");
    assert!(report.contains("could not list"), "{report}");
}

#[test]
fn a_non_executable_file_does_not_count_as_an_installed_program() {
    let programs = scratch_directory("not-executable");
    std::fs::write(programs.join("grim"), "#!/bin/sh\n").expect("write file");
    std::fs::set_permissions(
        programs.join("grim"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("drop execute bit");

    let output = run_doctor(Path::new("/nonexistent/umbriel.sock"), &programs);

    assert!(report(&output).contains("[fail] Screen capture (grim)"));
}

#[test]
#[ignore = "requires a running Umbriel session with grim and tesseract installed"]
fn a_ready_live_session_exits_successfully() {
    let output = Command::new(env!("CARGO_BIN_EXE_umbriel-mcp"))
        .arg("doctor")
        .output()
        .expect("run doctor");

    assert!(output.status.success(), "{}", report(&output));
    assert!(report(&output).contains("All 6 checks passed"));
}
