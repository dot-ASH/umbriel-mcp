use crate::update::{PackageManager, package_manager_of};
use anyhow::{Context, Result, anyhow};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstObjectProp, CstRootNode};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const SERVER_NAME: &str = "umbriel";
const SERVER_BINARY: &str = "umbriel-mcp";
const CLAUDE_REMOVAL: [&str; 5] = ["mcp", "remove", "--scope", "user", SERVER_NAME];

struct ConfigFile {
    harness: &'static str,
    path: PathBuf,
    servers_key: &'static str,
}

fn config_files(home: &Path, config_home: &Path) -> Vec<ConfigFile> {
    let file = |harness, path: PathBuf, servers_key| ConfigFile {
        harness,
        path,
        servers_key,
    };
    let mut files = vec![file("pi", home.join(".pi/agent/mcp.json"), "mcpServers")];
    for name in ["opencode.json", "opencode.jsonc", "config.json"] {
        files.push(file(
            "opencode",
            config_home.join("opencode").join(name),
            "mcp",
        ));
    }
    files.push(file("Cursor", home.join(".cursor/mcp.json"), "mcpServers"));
    for (harness, folder) in [
        ("VS Code", "Code"),
        ("VS Code", "Code - OSS"),
        ("VSCodium", "VSCodium"),
    ] {
        files.push(file(
            harness,
            config_home.join(folder).join("User/mcp.json"),
            "servers",
        ));
    }
    files.push(file(
        "Zed",
        config_home.join("zed/settings.json"),
        "context_servers",
    ));
    files
}

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    Ours,
    SomeoneElses,
    Absent,
    Unreadable(String),
}

fn umbriel_entry(root: &CstRootNode, servers_key: &str) -> Option<CstObjectProp> {
    root.object_value()?
        .object_value(servers_key)?
        .get(SERVER_NAME)
}

fn parse(text: &str) -> std::result::Result<CstRootNode, String> {
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|error| error.to_string())
}

pub fn entry_in(text: &str, servers_key: &str) -> Entry {
    let root = match parse(text) {
        Ok(root) => root,
        Err(error) => return Entry::Unreadable(error),
    };
    match umbriel_entry(&root, servers_key) {
        Some(entry) if entry.to_string().contains(SERVER_BINARY) => Entry::Ours,
        Some(_) => Entry::SomeoneElses,
        None => Entry::Absent,
    }
}

pub fn without_entry(text: &str, servers_key: &str) -> Result<String> {
    let root = parse(text).map_err(|error| anyhow!(error))?;
    umbriel_entry(&root, servers_key)
        .ok_or_else(|| anyhow!("the umbriel entry is gone"))?
        .remove();
    Ok(root.to_string())
}

fn write_in_place(path: &Path, text: &str) -> Result<()> {
    let target = std::fs::canonicalize(path)
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let staged = target.with_file_name(format!(
        ".{}.umbriel-mcp-{}",
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&staged, text).with_context(|| format!("cannot write {}", staged.display()))?;
    let permissions = std::fs::metadata(&target)?.permissions();
    std::fs::set_permissions(&staged, permissions)?;
    std::fs::rename(&staged, &target)
        .with_context(|| format!("cannot replace {}", target.display()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BinaryStep {
    Remove(PathBuf),
    Advise(String),
}

pub fn binary_step(invoked: &Path, resolved: &Path) -> BinaryStep {
    let shown = resolved.display();
    match package_manager_of(resolved) {
        Some(PackageManager::System) => BinaryStep::Advise(format!(
            "{shown} belongs to your system package manager; remove it there, for example \
             `sudo pacman -R umbriel-mcp-bin`."
        )),
        Some(PackageManager::Cargo) => BinaryStep::Advise(format!(
            "{shown} was installed with cargo; remove it with `cargo uninstall umbriel-mcp`."
        )),
        Some(PackageManager::Nix) => BinaryStep::Advise(format!(
            "{shown} comes from Nix; remove it from your Nix configuration."
        )),
        None => BinaryStep::Remove(invoked.to_path_buf()),
    }
}

fn found_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

fn invoked_executable() -> Result<(PathBuf, PathBuf)> {
    let resolved = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("cannot find the running umbriel-mcp executable")?;
    let first_argument = std::env::args_os().next().map(PathBuf::from);
    let invoked = first_argument.and_then(|argument| {
        if argument.components().count() > 1 {
            std::env::current_dir()
                .ok()
                .map(|folder| folder.join(argument))
        } else {
            found_on_path(&argument.to_string_lossy())
        }
    });
    let invoked = invoked
        .filter(|path| std::fs::canonicalize(path).ok().as_deref() == Some(resolved.as_path()))
        .unwrap_or_else(|| resolved.clone());
    Ok((invoked, resolved))
}

fn claude_lists_umbriel_mcp() -> bool {
    Command::new("claude")
        .args(["mcp", "get", SERVER_NAME])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(SERVER_BINARY)
        })
        .unwrap_or(false)
}

enum Step {
    EditFile(ConfigFile),
    Claude,
    RemoveBinary(PathBuf),
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::EditFile(file) => format!(
                "{}: the \"{SERVER_NAME}\" server in {}",
                file.harness,
                file.path.display()
            ),
            Step::Claude => {
                format!(
                    "Claude Code: the \"{SERVER_NAME}\" server (`claude {}`)",
                    CLAUDE_REMOVAL.join(" ")
                )
            }
            Step::RemoveBinary(path) => format!("the program {}", path.display()),
        }
    }

    fn perform(&self) -> Result<String> {
        match self {
            Step::EditFile(file) => {
                let text = std::fs::read_to_string(&file.path)?;
                write_in_place(&file.path, &without_entry(&text, file.servers_key)?)?;
                Ok(format!(
                    "Removed umbriel from {} ({}).",
                    file.harness,
                    file.path.display()
                ))
            }
            Step::Claude => {
                let _ = Command::new("claude")
                    .args(CLAUDE_REMOVAL)
                    .stdin(Stdio::null())
                    .output();
                if claude_lists_umbriel_mcp() {
                    return Err(anyhow!(
                        "Claude Code still lists umbriel, probably in a project or local scope; \
                         remove it by hand with `claude mcp remove umbriel`."
                    ));
                }
                Ok("Removed umbriel from Claude Code.".to_owned())
            }
            Step::RemoveBinary(path) => match std::fs::remove_file(path) {
                Ok(()) => Ok(format!("Removed {}.", path.display())),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(anyhow!(
                    "cannot remove {} because its folder is not writable by you; run `sudo rm {}`.",
                    path.display(),
                    path.display()
                )),
                Err(error) => Err(anyhow!("cannot remove {}: {error}", path.display())),
            },
        }
    }
}

struct Plan {
    steps: Vec<Step>,
    by_hand: Vec<String>,
    advice: Vec<String>,
}

fn plan(home: &Path, config_home: &Path, keep_binary: bool) -> Result<Plan> {
    let mut plan = Plan {
        steps: Vec::new(),
        by_hand: Vec::new(),
        advice: Vec::new(),
    };
    for file in config_files(home, config_home) {
        let Ok(text) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        match entry_in(&text, file.servers_key) {
            Entry::Ours => plan.steps.push(Step::EditFile(file)),
            Entry::Unreadable(error) if text.contains(SERVER_BINARY) => plan.by_hand.push(format!(
                "{}: {} mentions umbriel-mcp but could not be read ({error}); remove the \
                 \"{SERVER_NAME}\" entry under \"{}\" by hand.",
                file.harness,
                file.path.display(),
                file.servers_key
            )),
            _ => {}
        }
    }
    if found_on_path("claude").is_some() && claude_lists_umbriel_mcp() {
        plan.steps.push(Step::Claude);
    }
    if !keep_binary {
        let (invoked, resolved) = invoked_executable()?;
        match binary_step(&invoked, &resolved) {
            BinaryStep::Remove(path) => plan.steps.push(Step::RemoveBinary(path)),
            BinaryStep::Advise(advice) => plan.advice.push(advice),
        }
    }
    Ok(plan)
}

fn leftovers(config_home: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let umbriel_folder = config_home.join("umbriel");
    let backups = std::fs::read_dir(&umbriel_folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            crate::config_edit::backup_order(&entry.file_name().to_string_lossy(), "config.toml")
                .is_some()
        })
        .count();
    if backups > 0 {
        let plural = if backups == 1 { "backup" } else { "backups" };
        found.push(format!(
            "{backups} config {plural} made by umbriel-mcp: {}/config.toml.bak-*",
            umbriel_folder.display()
        ));
    }
    if let Ok(recordings) = crate::recording::recordings_location()
        && std::fs::read_dir(&recordings).is_ok_and(|mut entries| entries.next().is_some())
    {
        found.push(format!("screen recordings in {}", recordings.display()));
    }
    found
}

fn confirmed(assume_yes: bool) -> Option<bool> {
    if assume_yes {
        return Some(true);
    }
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("Remove these? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn uninstall(assume_yes: bool, keep_binary: bool) -> Result<bool> {
    let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let plan = plan(&home, &config_home, keep_binary)?;
    let registered = plan
        .steps
        .iter()
        .any(|step| !matches!(step, Step::RemoveBinary(_)));
    if !registered && plan.by_hand.is_empty() {
        println!("umbriel-mcp is not registered with any harness.");
    }
    let mut succeeded = true;
    if !plan.steps.is_empty() {
        println!("This removes:");
        for step in &plan.steps {
            println!("  - {}", step.describe());
        }
        match confirmed(assume_yes) {
            Some(true) => {
                for step in &plan.steps {
                    match step.perform() {
                        Ok(report) => println!("{report}"),
                        Err(error) => {
                            succeeded = false;
                            eprintln!("{error:#}");
                        }
                    }
                }
            }
            Some(false) => {
                println!("Nothing was changed.");
                return Ok(true);
            }
            None => {
                println!(
                    "Nothing was changed. Run `umbriel-mcp uninstall --yes` to remove them \
                     without being asked."
                );
                return Ok(false);
            }
        }
    }
    for item in &plan.by_hand {
        succeeded = false;
        eprintln!("{item}");
    }
    for advice in &plan.advice {
        println!("{advice}");
    }
    let kept = leftovers(&config_home);
    if !kept.is_empty() {
        println!("Left in place; delete them yourself if you no longer want them:");
        for item in kept {
            println!("  - {item}");
        }
    }
    if registered {
        println!("Restart any harness that is running so it stops starting umbriel-mcp.");
    }
    Ok(succeeded)
}

pub fn run(assume_yes: bool, keep_binary: bool) -> ExitCode {
    match uninstall(assume_yes, keep_binary) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BinaryStep, Entry, binary_step, entry_in, without_entry};
    use std::path::Path;

    #[test]
    fn only_an_entry_that_runs_umbriel_mcp_counts_as_ours() {
        let ours = r#"{"mcp": {"umbriel": {"command": ["/home/u/.local/bin/umbriel-mcp"]}}}"#;
        let theirs = r#"{"mcp": {"umbriel": {"command": ["other"]}}}"#;
        assert_eq!(entry_in(ours, "mcp"), Entry::Ours);
        assert_eq!(entry_in(theirs, "mcp"), Entry::SomeoneElses);
        assert_eq!(entry_in(r#"{"mcp": {}}"#, "mcp"), Entry::Absent);
        assert_eq!(entry_in(r#"{"other": 1}"#, "mcp"), Entry::Absent);
        assert!(matches!(entry_in("{ broken", "mcp"), Entry::Unreadable(_)));
    }

    #[test]
    fn removing_the_entry_keeps_comments_and_neighbours() {
        let text = "{\n  // keep me\n  \"mcp\": {\n    \"a\": 1,\n    \"umbriel\": { \"command\": \"umbriel-mcp\" }\n  }\n}\n";
        let edited = without_entry(text, "mcp").unwrap();
        assert!(
            edited.contains("// keep me") && edited.contains("\"a\": 1"),
            "{edited}"
        );
        assert!(!edited.contains("umbriel"), "{edited}");
    }

    #[test]
    fn package_managed_binaries_are_left_to_their_package_manager() {
        let advise = |path: &str| match binary_step(Path::new(path), Path::new(path)) {
            BinaryStep::Advise(advice) => advice,
            BinaryStep::Remove(_) => panic!("{path} should not be removed"),
        };
        assert!(advise("/usr/bin/umbriel-mcp").contains("pacman -R"));
        assert!(advise("/home/u/.cargo/bin/umbriel-mcp").contains("cargo uninstall"));
        assert!(advise("/nix/store/x/bin/umbriel-mcp").contains("Nix"));
    }

    #[test]
    fn a_symlink_is_removed_rather_than_what_it_points_to() {
        let link = Path::new("/home/u/.local/bin/umbriel-mcp");
        let target = Path::new("/home/u/src/umbriel-mcp/target/release/umbriel-mcp");
        assert_eq!(
            binary_step(link, target),
            BinaryStep::Remove(link.to_path_buf())
        );
    }
}
