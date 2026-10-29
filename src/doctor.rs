use crate::{input, ipc};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitCode};

const SOCKET_CHECK: &str = "Umbriel socket";
const POINTER_CHECK: &str = "Virtual pointer protocol";
const KEYBOARD_CHECK: &str = "Virtual keyboard protocol";
const GRIM_CHECK: &str = "Screen capture (grim)";
const TESSERACT_CHECK: &str = "OCR (tesseract)";
const ENGLISH_DATA_CHECK: &str = "OCR English data";

const TESSERACT_LIST_HEADER_PREFIX: &str = "List of available languages";
const ENGLISH_LANGUAGE_CODE: &str = "eng";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    Arch,
    Debian,
    Fedora,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolGlobals {
    pub virtual_pointer: bool,
    pub virtual_keyboard: bool,
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub compositor_socket: Result<String, String>,
    pub protocols: Result<ProtocolGlobals, String>,
    pub grim_installed: bool,
    pub tesseract_installed: bool,
    pub tesseract_languages: Option<Vec<String>>,
    pub distribution: Distribution,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Ready(String),
    Failing { problem: String, fix: String },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub outcome: Outcome,
}

impl Check {
    fn ready(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            outcome: Outcome::Ready(detail.into()),
        }
    }

    fn failing(name: &'static str, problem: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            outcome: Outcome::Failing {
                problem: problem.into(),
                fix: fix.into(),
            },
        }
    }
}

pub fn parse_distribution(os_release: &str) -> Distribution {
    let field = |wanted: &str| -> Vec<String> {
        os_release
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(key, _)| *key == wanted)
            .flat_map(|(_, value)| {
                value
                    .trim_matches('"')
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let family: Vec<String> = field("ID").into_iter().chain(field("ID_LIKE")).collect();
    let belongs_to = |names: &[&str]| family.iter().any(|member| names.contains(&member.as_str()));
    if belongs_to(&["arch"]) {
        Distribution::Arch
    } else if belongs_to(&["debian", "ubuntu"]) {
        Distribution::Debian
    } else if belongs_to(&["fedora"]) {
        Distribution::Fedora
    } else {
        Distribution::Unknown
    }
}

pub fn parse_tesseract_languages(list_langs_output: &str) -> Vec<String> {
    list_langs_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(TESSERACT_LIST_HEADER_PREFIX))
        .map(str::to_owned)
        .collect()
}

#[derive(Clone, Copy)]
enum Prerequisite {
    Grim,
    Tesseract,
    EnglishData,
}

struct PackageNames {
    install_command: &'static str,
    grim: &'static str,
    tesseract: &'static str,
    english_data: &'static str,
}

impl PackageNames {
    fn for_distribution(distribution: Distribution) -> Option<Self> {
        match distribution {
            Distribution::Arch => Some(Self {
                install_command: "sudo pacman -S",
                grim: "grim",
                tesseract: "tesseract",
                english_data: "tesseract-data-eng",
            }),
            Distribution::Debian => Some(Self {
                install_command: "sudo apt install",
                grim: "grim",
                tesseract: "tesseract-ocr",
                english_data: "tesseract-ocr-eng",
            }),
            Distribution::Fedora => Some(Self {
                install_command: "sudo dnf install",
                grim: "grim",
                tesseract: "tesseract",
                english_data: "tesseract-langpack-eng",
            }),
            Distribution::Unknown => None,
        }
    }

    fn command_for(&self, prerequisite: Prerequisite) -> String {
        let package = match prerequisite {
            Prerequisite::Grim => self.grim,
            Prerequisite::Tesseract => self.tesseract,
            Prerequisite::EnglishData => self.english_data,
        };
        format!("{} {package}", self.install_command)
    }
}

fn generic_installation_advice(prerequisite: Prerequisite) -> &'static str {
    match prerequisite {
        Prerequisite::Grim => {
            "Install grim, the Wayland screenshot tool, using your distribution's package manager, and make sure it is on PATH."
        }
        Prerequisite::Tesseract => {
            "Install the Tesseract OCR engine (the tesseract program) using your distribution's package manager, and make sure it is on PATH."
        }
        Prerequisite::EnglishData => {
            "Install Tesseract's English language data (eng.traineddata) using your distribution's package manager, or place the file in Tesseract's tessdata directory."
        }
    }
}

fn how_to_install(distribution: Distribution, prerequisite: Prerequisite) -> String {
    match PackageNames::for_distribution(distribution) {
        Some(packages) => packages.command_for(prerequisite),
        None => generic_installation_advice(prerequisite).to_owned(),
    }
}

fn socket_check(facts: &Facts) -> Check {
    match &facts.compositor_socket {
        Ok(path) => Check::ready(SOCKET_CHECK, format!("reachable at {path}")),
        Err(problem) => Check::failing(
            SOCKET_CHECK,
            problem.clone(),
            "Start Umbriel, or set UMBRIEL_SOCKET to the path of its control socket.",
        ),
    }
}

fn protocol_check(
    name: &'static str,
    interface: &str,
    facts: &Facts,
    is_available: impl Fn(&ProtocolGlobals) -> bool,
) -> Check {
    match &facts.protocols {
        Ok(globals) if is_available(globals) => {
            Check::ready(name, format!("{interface} is advertised"))
        }
        Ok(_) => Check::failing(
            name,
            format!("Umbriel does not advertise {interface}"),
            format!(
                "Use an Umbriel build that provides {interface}, and run this server inside that session rather than a restricted sandbox."
            ),
        ),
        Err(problem) => Check::failing(
            name,
            problem.clone(),
            "Run the MCP server inside the Umbriel session: check that WAYLAND_DISPLAY and XDG_RUNTIME_DIR are set.",
        ),
    }
}

fn program_check(
    name: &'static str,
    program: &str,
    installed: bool,
    distribution: Distribution,
    prerequisite: Prerequisite,
) -> Check {
    if installed {
        Check::ready(name, format!("{program} found on PATH"))
    } else {
        Check::failing(
            name,
            format!("{program} was not found on PATH"),
            how_to_install(distribution, prerequisite),
        )
    }
}

fn english_data_check(facts: &Facts) -> Check {
    let fix = how_to_install(facts.distribution, Prerequisite::EnglishData);
    match &facts.tesseract_languages {
        Some(languages) if languages.iter().any(|code| code == ENGLISH_LANGUAGE_CODE) => {
            Check::ready(ENGLISH_DATA_CHECK, "English (eng) is installed")
        }
        Some(_) => Check::failing(
            ENGLISH_DATA_CHECK,
            "tesseract has no English (eng) language data",
            fix,
        ),
        None if !facts.tesseract_installed => Check::failing(
            ENGLISH_DATA_CHECK,
            "cannot check language data because tesseract is not installed",
            fix,
        ),
        None => Check::failing(
            ENGLISH_DATA_CHECK,
            "could not list tesseract's languages (`tesseract --list-langs` failed)",
            fix,
        ),
    }
}

pub fn diagnose(facts: &Facts) -> Vec<Check> {
    vec![
        socket_check(facts),
        protocol_check(
            POINTER_CHECK,
            "zwlr_virtual_pointer_manager_v1",
            facts,
            |globals| globals.virtual_pointer,
        ),
        protocol_check(
            KEYBOARD_CHECK,
            "zwp_virtual_keyboard_manager_v1",
            facts,
            |globals| globals.virtual_keyboard,
        ),
        program_check(
            GRIM_CHECK,
            "grim",
            facts.grim_installed,
            facts.distribution,
            Prerequisite::Grim,
        ),
        program_check(
            TESSERACT_CHECK,
            "tesseract",
            facts.tesseract_installed,
            facts.distribution,
            Prerequisite::Tesseract,
        ),
        english_data_check(facts),
    ]
}

pub fn all_ready(checks: &[Check]) -> bool {
    checks
        .iter()
        .all(|check| matches!(check.outcome, Outcome::Ready(_)))
}

pub fn render(checks: &[Check]) -> String {
    let mut report = String::from("umbriel-mcp doctor\n\n");
    for check in checks {
        match &check.outcome {
            Outcome::Ready(detail) => {
                report.push_str(&format!("[ok]   {}: {detail}\n", check.name));
            }
            Outcome::Failing { problem, fix } => {
                report.push_str(&format!("[fail] {}: {problem}\n", check.name));
                report.push_str(&format!("       fix: {fix}\n"));
            }
        }
    }
    let failed = checks
        .iter()
        .filter(|check| matches!(check.outcome, Outcome::Failing { .. }))
        .count();
    if failed == 0 {
        report.push_str(&format!("\nAll {} checks passed.\n", checks.len()));
    } else {
        report.push_str(&format!("\n{failed} of {} checks failed.\n", checks.len()));
    }
    report
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn is_on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|search_path| {
            std::env::split_paths(&search_path)
                .any(|directory| is_executable_file(&directory.join(program)))
        })
        .unwrap_or(false)
}

fn list_tesseract_languages() -> Option<Vec<String>> {
    let output = Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Some(parse_tesseract_languages(&listing))
}

fn compositor_socket_fact() -> Result<String, String> {
    ipc::query("outputs").map_err(|error| format!("{error:#}"))?;
    ipc::socket_path().map_err(|error| format!("{error:#}"))
}

fn protocol_fact() -> Result<ProtocolGlobals, String> {
    input::input_protocol_support()
        .map(|(virtual_pointer, virtual_keyboard)| ProtocolGlobals {
            virtual_pointer,
            virtual_keyboard,
        })
        .map_err(|error| format!("{error:#}"))
}

fn gather_facts() -> Facts {
    let tesseract_installed = is_on_path("tesseract");
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    Facts {
        compositor_socket: compositor_socket_fact(),
        protocols: protocol_fact(),
        grim_installed: is_on_path("grim"),
        tesseract_installed,
        tesseract_languages: tesseract_installed.then(list_tesseract_languages).flatten(),
        distribution: parse_distribution(&os_release),
    }
}

pub fn run() -> ExitCode {
    let checks = diagnose(&gather_facts());
    print!("{}", render(&checks));
    if all_ready(&checks) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Check, Distribution, Facts, Outcome, ProtocolGlobals, all_ready, diagnose,
        parse_distribution, parse_tesseract_languages, render,
    };

    fn healthy_facts() -> Facts {
        Facts {
            compositor_socket: Ok("/run/user/1000/umbriel-wayland-0.sock".to_owned()),
            protocols: Ok(ProtocolGlobals {
                virtual_pointer: true,
                virtual_keyboard: true,
            }),
            grim_installed: true,
            tesseract_installed: true,
            tesseract_languages: Some(vec!["eng".to_owned(), "osd".to_owned()]),
            distribution: Distribution::Arch,
        }
    }

    fn check<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("no check named {name:?} in {checks:?}"))
    }

    fn failing_fix(checks: &[Check], name: &str) -> String {
        match &check(checks, name).outcome {
            Outcome::Failing { fix, .. } => fix.clone(),
            Outcome::Ready(_) => panic!("{name} should be failing"),
        }
    }

    fn failing_names(checks: &[Check]) -> Vec<&'static str> {
        checks
            .iter()
            .filter(|check| matches!(check.outcome, Outcome::Failing { .. }))
            .map(|check| check.name)
            .collect()
    }

    #[test]
    fn a_fully_equipped_session_is_ready() {
        let checks = diagnose(&healthy_facts());

        assert!(all_ready(&checks), "{}", render(&checks));
        assert_eq!(failing_names(&checks), Vec::<&str>::new());
    }

    #[test]
    fn an_unreachable_compositor_fails_only_the_socket_check() {
        let mut facts = healthy_facts();
        facts.compositor_socket = Err("cannot reach umbriel at /nowhere".to_owned());

        let checks = diagnose(&facts);

        assert!(!all_ready(&checks));
        assert_eq!(failing_names(&checks), vec!["Umbriel socket"]);
        let fix = failing_fix(&checks, "Umbriel socket");
        assert!(fix.contains("UMBRIEL_SOCKET"), "{fix}");
    }

    #[test]
    fn a_missing_pointer_protocol_does_not_blame_the_keyboard() {
        let mut facts = healthy_facts();
        facts.protocols = Ok(ProtocolGlobals {
            virtual_pointer: false,
            virtual_keyboard: true,
        });

        let checks = diagnose(&facts);

        assert_eq!(failing_names(&checks), vec!["Virtual pointer protocol"]);
        let fix = failing_fix(&checks, "Virtual pointer protocol");
        assert!(fix.contains("zwlr_virtual_pointer_manager_v1"), "{fix}");
    }

    #[test]
    fn a_missing_keyboard_protocol_does_not_blame_the_pointer() {
        let mut facts = healthy_facts();
        facts.protocols = Ok(ProtocolGlobals {
            virtual_pointer: true,
            virtual_keyboard: false,
        });

        let checks = diagnose(&facts);

        assert_eq!(failing_names(&checks), vec!["Virtual keyboard protocol"]);
        let fix = failing_fix(&checks, "Virtual keyboard protocol");
        assert!(fix.contains("zwp_virtual_keyboard_manager_v1"), "{fix}");
    }

    #[test]
    fn a_wayland_connection_failure_fails_both_protocol_checks_with_the_reason() {
        let mut facts = healthy_facts();
        facts.protocols = Err("cannot connect to Wayland".to_owned());

        let checks = diagnose(&facts);

        assert_eq!(
            failing_names(&checks),
            vec!["Virtual pointer protocol", "Virtual keyboard protocol"]
        );
        for name in ["Virtual pointer protocol", "Virtual keyboard protocol"] {
            match &check(&checks, name).outcome {
                Outcome::Failing { problem, fix } => {
                    assert!(problem.contains("cannot connect to Wayland"), "{problem}");
                    assert!(fix.contains("WAYLAND_DISPLAY"), "{fix}");
                }
                Outcome::Ready(_) => panic!("{name} should fail"),
            }
        }
    }

    #[test]
    fn a_missing_screenshot_tool_names_the_package_for_each_known_distribution() {
        for (distribution, expected) in [
            (Distribution::Arch, "pacman -S grim"),
            (Distribution::Debian, "apt install grim"),
            (Distribution::Fedora, "dnf install grim"),
        ] {
            let mut facts = healthy_facts();
            facts.grim_installed = false;
            facts.distribution = distribution;

            let checks = diagnose(&facts);

            assert_eq!(failing_names(&checks), vec!["Screen capture (grim)"]);
            let fix = failing_fix(&checks, "Screen capture (grim)");
            assert!(fix.contains(expected), "{distribution:?}: {fix}");
        }
    }

    #[test]
    fn an_unknown_distribution_gets_generic_guidance_without_guessed_package_names() {
        let mut facts = healthy_facts();
        facts.grim_installed = false;
        facts.tesseract_installed = false;
        facts.tesseract_languages = None;
        facts.distribution = Distribution::Unknown;

        let checks = diagnose(&facts);

        for name in [
            "Screen capture (grim)",
            "OCR (tesseract)",
            "OCR English data",
        ] {
            let fix = failing_fix(&checks, name);
            for guessed_manager in ["pacman", "apt", "dnf", "zypper", "nix"] {
                assert!(
                    !fix.contains(guessed_manager),
                    "{name} must not guess a package manager: {fix}"
                );
            }
            assert!(fix.len() > 20, "{name} needs useful guidance: {fix}");
        }
        let grim_fix = failing_fix(&checks, "Screen capture (grim)");
        assert!(grim_fix.contains("grim"), "{grim_fix}");
        let data_fix = failing_fix(&checks, "OCR English data");
        assert!(data_fix.contains("eng.traineddata"), "{data_fix}");
    }

    #[test]
    fn missing_tesseract_fails_both_the_program_and_its_language_data() {
        let mut facts = healthy_facts();
        facts.tesseract_installed = false;
        facts.tesseract_languages = None;

        let checks = diagnose(&facts);

        assert_eq!(
            failing_names(&checks),
            vec!["OCR (tesseract)", "OCR English data"]
        );
        assert!(failing_fix(&checks, "OCR (tesseract)").contains("pacman -S tesseract"));
        assert!(failing_fix(&checks, "OCR English data").contains("tesseract-data-eng"));
    }

    #[test]
    fn missing_english_data_names_the_language_package_for_each_known_distribution() {
        for (distribution, expected) in [
            (Distribution::Arch, "tesseract-data-eng"),
            (Distribution::Debian, "tesseract-ocr-eng"),
            (Distribution::Fedora, "tesseract-langpack-eng"),
        ] {
            let mut facts = healthy_facts();
            facts.tesseract_languages = Some(vec!["osd".to_owned(), "afr".to_owned()]);
            facts.distribution = distribution;

            let checks = diagnose(&facts);

            assert_eq!(failing_names(&checks), vec!["OCR English data"]);
            let fix = failing_fix(&checks, "OCR English data");
            assert!(fix.contains(expected), "{distribution:?}: {fix}");
        }
    }

    #[test]
    fn languages_that_merely_start_with_eng_do_not_count_as_english() {
        let mut facts = healthy_facts();
        facts.tesseract_languages = Some(vec!["engfake".to_owned(), "eng_best".to_owned()]);

        assert_eq!(failing_names(&diagnose(&facts)), vec!["OCR English data"]);
    }

    #[test]
    fn unlistable_languages_fail_the_data_check_with_a_clear_problem() {
        let mut facts = healthy_facts();
        facts.tesseract_languages = None;

        let checks = diagnose(&facts);

        assert_eq!(failing_names(&checks), vec!["OCR English data"]);
        match &check(&checks, "OCR English data").outcome {
            Outcome::Failing { problem, .. } => {
                assert!(problem.contains("list"), "{problem}")
            }
            Outcome::Ready(_) => panic!("should fail"),
        }
    }

    #[test]
    fn independent_failures_are_all_reported() {
        let mut facts = healthy_facts();
        facts.compositor_socket = Err("down".to_owned());
        facts.grim_installed = false;

        let checks = diagnose(&facts);

        assert_eq!(
            failing_names(&checks),
            vec!["Umbriel socket", "Screen capture (grim)"]
        );
    }

    #[test]
    fn the_report_lists_every_check_with_its_fix_and_a_summary() {
        let mut facts = healthy_facts();
        facts.grim_installed = false;

        let report = render(&diagnose(&facts));

        assert!(report.contains("[ok]"), "{report}");
        assert!(report.contains("[fail] Screen capture (grim)"), "{report}");
        assert!(report.contains("fix: sudo pacman -S grim"), "{report}");
        assert!(report.contains("1 of 6 checks failed"), "{report}");
    }

    #[test]
    fn the_report_celebrates_a_fully_ready_session() {
        let report = render(&diagnose(&healthy_facts()));

        assert!(!report.contains("[fail]"), "{report}");
        assert!(report.contains("All 6 checks passed"), "{report}");
    }

    #[test]
    fn distributions_are_recognised_from_id_and_id_like() {
        let cases = [
            ("ID=arch\n", Distribution::Arch),
            ("ID=cachyos\nID_LIKE=arch\n", Distribution::Arch),
            ("ID=ubuntu\nID_LIKE=debian\n", Distribution::Debian),
            ("ID=debian\n", Distribution::Debian),
            ("ID=\"fedora\"\n", Distribution::Fedora),
            (
                "ID=nobara\nID_LIKE=\"rhel centos fedora\"\n",
                Distribution::Fedora,
            ),
            ("ID=nixos\n", Distribution::Unknown),
            ("NAME=Something\n", Distribution::Unknown),
            ("", Distribution::Unknown),
        ];
        for (os_release, expected) in cases {
            assert_eq!(parse_distribution(os_release), expected, "{os_release:?}");
        }
    }

    #[test]
    fn tesseract_language_listings_drop_the_header() {
        let output =
            "List of available languages in \"/usr/share/tessdata/\" (3):\nafr\neng\nosd\n";

        assert_eq!(parse_tesseract_languages(output), vec!["afr", "eng", "osd"]);
        assert_eq!(parse_tesseract_languages(""), Vec::<String>::new());
    }
}
