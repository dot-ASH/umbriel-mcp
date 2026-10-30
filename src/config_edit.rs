use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

const KEYBINDS_TABLE: &str = "keybinds";
const ACTION_FIELD: &str = "action";
const REPEAT_FIELD: &str = "repeat";

#[derive(Debug, PartialEq, Eq)]
pub struct KeybindEdit {
    pub chord: String,
    pub new_text: String,
    pub previous: Option<String>,
}

fn normalized_chord(chord: &str) -> Vec<String> {
    let mut parts: Vec<String> = chord
        .split('+')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    let key = parts.pop();
    parts.sort();
    parts.extend(key);
    parts
}

pub fn same_chord(left: &str, right: &str) -> bool {
    normalized_chord(left) == normalized_chord(right)
}

fn parse(config: &str) -> Result<DocumentMut> {
    config
        .parse::<DocumentMut>()
        .context("the Umbriel config is not valid TOML, so it was not changed")
}

fn keybinds_table(document: &mut DocumentMut) -> Result<&mut dyn TableLike> {
    if document.get(KEYBINDS_TABLE).is_none() {
        let mut table = Table::new();
        table.decor_mut().set_prefix("\n");
        document.insert(KEYBINDS_TABLE, Item::Table(table));
    }
    document
        .get_mut(KEYBINDS_TABLE)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow!("`keybinds` in the Umbriel config is not a table"))
}

fn existing_chord(table: &dyn TableLike, chord: &str) -> Option<String> {
    table
        .iter()
        .map(|(key, _)| key.to_owned())
        .find(|key| same_chord(key, chord))
}

fn bound_action(item: &Item) -> Option<String> {
    match item.as_value()? {
        Value::String(action) => Some(action.value().clone()),
        Value::InlineTable(table) => table
            .get(ACTION_FIELD)
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn require_text<'a>(value: &'a str, what: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("the {what} must not be empty"));
    }
    Ok(value)
}

fn replacement_value(existing: Option<&Value>, action: &str, repeat: Option<bool>) -> Value {
    let decor = existing.map(|value| value.decor().clone());
    let mut value = match existing {
        Some(Value::InlineTable(table)) => {
            let mut table = table.clone();
            replace_field(&mut table, ACTION_FIELD, Value::from(action));
            if let Some(repeat) = repeat {
                replace_field(&mut table, REPEAT_FIELD, Value::from(repeat));
            }
            Value::InlineTable(table)
        }
        _ => match repeat {
            Some(repeat) => {
                let mut table = InlineTable::new();
                table.insert(ACTION_FIELD, Value::from(action));
                table.insert(REPEAT_FIELD, Value::from(repeat));
                table.fmt();
                Value::InlineTable(table)
            }
            None => Value::from(action),
        },
    };
    if let Some(decor) = decor {
        *value.decor_mut() = decor;
    }
    value
}

fn replace_field(table: &mut InlineTable, field: &str, mut replacement: Value) {
    if let Some(existing) = table.get(field) {
        *replacement.decor_mut() = existing.decor().clone();
        table.insert(field, replacement);
    } else {
        table.insert(field, replacement);
        table.fmt();
    }
}

pub fn plan_set_keybind(
    config: &str,
    chord: &str,
    action: &str,
    repeat: Option<bool>,
) -> Result<KeybindEdit> {
    let chord = require_text(chord, "key chord")?;
    let action = require_text(action, "action")?;
    let mut document = parse(config)?;
    let table = keybinds_table(&mut document)?;
    let (key, previous) = match existing_chord(table, chord) {
        Some(key) => {
            let item = table.get_mut(&key).expect("chord found above");
            let previous = bound_action(item);
            let value = replacement_value(item.as_value(), action, repeat);
            *item = Item::Value(value);
            (key, previous)
        }
        None => {
            table.insert(chord, Item::Value(replacement_value(None, action, repeat)));
            (chord.to_owned(), None)
        }
    };
    Ok(KeybindEdit {
        chord: key,
        new_text: document.to_string(),
        previous,
    })
}

pub fn plan_remove_keybind(config: &str, chord: &str) -> Result<KeybindEdit> {
    let chord = require_text(chord, "key chord")?;
    let mut document = parse(config)?;
    let not_bound =
        || anyhow!("{chord} is not bound in the Umbriel config, so nothing was removed");
    let table = document
        .get_mut(KEYBINDS_TABLE)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(not_bound)?;
    let key = existing_chord(table, chord).ok_or_else(not_bound)?;
    let leading_comments = table
        .key(&key)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .map(str::to_owned)
        .unwrap_or_default();
    let next_key = table
        .iter()
        .map(|(name, _)| name.to_owned())
        .skip_while(|name| name != &key)
        .nth(1);
    let previous = table.remove(&key).as_ref().and_then(bound_action);
    if let Some(next_key) = next_key
        && let Some(mut next) = table.key_mut(&next_key)
    {
        let next_prefix = next
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .to_owned();
        next.leaf_decor_mut()
            .set_prefix(format!("{leading_comments}{next_prefix}"));
    }
    Ok(KeybindEdit {
        chord: key,
        new_text: document.to_string(),
        previous,
    })
}

pub fn changed_lines(before: &str, after: &str) -> String {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let common_prefix = old
        .iter()
        .zip(&new)
        .take_while(|(left, right)| left == right)
        .count();
    let common_suffix = old[common_prefix..]
        .iter()
        .rev()
        .zip(new[common_prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let removed = &old[common_prefix..old.len() - common_suffix];
    let added = &new[common_prefix..new.len() - common_suffix];
    removed
        .iter()
        .map(|line| format!("- {line}"))
        .chain(added.iter().map(|line| format!("+ {line}")))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn included_files(config: &str) -> Vec<String> {
    let Ok(document) = config.parse::<DocumentMut>() else {
        return Vec::new();
    };
    let files_in = |item: Option<&Item>| -> Vec<String> {
        item.and_then(|item| item.get("files"))
            .and_then(Item::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let include = document.get("include");
    let mut files = files_in(include);
    files.extend(files_in(
        include.and_then(|include| include.get("optional")),
    ));
    files
}

fn user_config_file() -> Result<PathBuf> {
    let path = crate::compositor::user_config_path(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .ok_or_else(|| {
        anyhow!("cannot locate the Umbriel config: neither XDG_CONFIG_HOME nor HOME is set")
    })?;
    if !path.is_file() {
        return Err(anyhow!(
            "there is no user config at {}; Umbriel is running on its packaged defaults. Copy \
             /usr/share/umbriel/config.toml to {} first, then retry.",
            path.display(),
            path.display()
        ));
    }
    std::fs::canonicalize(&path).with_context(|| format!("cannot resolve {}", path.display()))
}

fn resolve_included(config_file: &Path, listed: &str) -> PathBuf {
    if let Some(relative_to_home) = listed.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(relative_to_home);
    }
    let listed = PathBuf::from(listed);
    if listed.is_absolute() {
        return listed;
    }
    config_file
        .parent()
        .map(|directory| directory.join(&listed))
        .unwrap_or(listed)
}

fn binds_chord(config: &str, chord: &str) -> bool {
    config
        .parse::<DocumentMut>()
        .ok()
        .and_then(|document| {
            document
                .get(KEYBINDS_TABLE)
                .and_then(Item::as_table_like)
                .map(|table| existing_chord(table, chord).is_some())
        })
        .unwrap_or(false)
}

fn refuse_chords_owned_by_included_files(
    config_file: &Path,
    config: &str,
    chord: &str,
) -> Result<()> {
    for listed in included_files(config) {
        let included = resolve_included(config_file, &listed);
        if let Ok(text) = std::fs::read_to_string(&included)
            && binds_chord(&text, chord)
        {
            return Err(anyhow!(
                "{chord} is bound in the included file {}; editing the main config would leave \
                 two bindings for it, so nothing was changed. Edit that file instead.",
                included.display()
            ));
        }
    }
    Ok(())
}

fn readable_validation_report(raw: &str, candidate: &Path, config_file: &Path) -> String {
    let candidate_name = candidate.display().to_string();
    let config_name = config_file.display().to_string();
    raw.lines()
        .filter(|line| !line.contains('\u{1b}') && !line.trim().is_empty())
        .map(|line| line.replace(&candidate_name, &config_name))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate(config_file: &Path) -> Result<std::result::Result<(), String>> {
    let output = Command::new("umbriel")
        .args(["config", "validate", "-c"])
        .arg(config_file)
        .output()
        .map_err(|cause| {
            anyhow!("cannot run `umbriel config validate` ({cause}), so the config was not changed")
        })?;
    let report = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
    Ok(if output.status.success() {
        Ok(())
    } else {
        Err(report)
    })
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn sibling(config_file: &Path, suffix: &str) -> PathBuf {
    let name = config_file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_owned());
    config_file.with_file_name(format!("{name}{suffix}"))
}

fn write_new_backup(config_file: &Path, original: &str) -> Result<PathBuf> {
    use std::io::Write as _;
    let stamp = unix_seconds();
    for attempt in 1.. {
        let suffix = if attempt == 1 {
            format!(".bak-{stamp}")
        } else {
            format!(".bak-{stamp}-{attempt}")
        };
        let backup = sibling(config_file, &suffix);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(original.as_bytes())
                    .with_context(|| format!("cannot write the backup {}", backup.display()))?;
                if let Ok(metadata) = std::fs::metadata(config_file) {
                    let _ = std::fs::set_permissions(&backup, metadata.permissions());
                }
                return Ok(backup);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot create the backup {}", backup.display()));
            }
        }
    }
    unreachable!("an unused backup name is always found")
}

struct AppliedEdit {
    edit: KeybindEdit,
    config_file: PathBuf,
    backup: PathBuf,
    change: String,
}

fn apply(
    chord: &str,
    plan: impl FnOnce(&str) -> Result<KeybindEdit>,
) -> Result<Option<AppliedEdit>> {
    let config_file = user_config_file()?;
    let original = std::fs::read_to_string(&config_file)
        .with_context(|| format!("cannot read {}", config_file.display()))?;
    refuse_chords_owned_by_included_files(&config_file, &original, chord)?;
    let edit = plan(&original)?;
    if edit.new_text == original {
        return Ok(None);
    }

    let candidate = config_file.with_file_name(format!(
        ".{}.umbriel-mcp-{}",
        config_file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&candidate, &edit.new_text)
        .with_context(|| format!("cannot write {}", candidate.display()))?;
    let verdict = validate(&candidate);
    let rejection = match verdict {
        Ok(Ok(())) => None,
        Ok(Err(report)) => Some(readable_validation_report(
            &report,
            &candidate,
            &config_file,
        )),
        Err(error) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(error);
        }
    };
    if let Some(report) = rejection {
        let _ = std::fs::remove_file(&candidate);
        let already_invalid = matches!(validate(&config_file), Ok(Err(_)));
        return Err(if already_invalid {
            anyhow!(
                "{} already fails `umbriel config validate`, so the change could not be checked \
                 and nothing was written. Fix the existing problems first:\n{report}",
                config_file.display()
            )
        } else {
            anyhow!(
                "Umbriel rejected the change, so {} was not modified:\n{report}",
                config_file.display()
            )
        });
    }

    let finish = || -> Result<PathBuf> {
        let backup = write_new_backup(&config_file, &original)?;
        let permissions = std::fs::metadata(&config_file)?.permissions();
        std::fs::set_permissions(&candidate, permissions)?;
        std::fs::rename(&candidate, &config_file)
            .with_context(|| format!("cannot replace {}", config_file.display()))?;
        Ok(backup)
    };
    let backup = match finish() {
        Ok(backup) => backup,
        Err(error) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(error);
        }
    };
    let change = changed_lines(&original, &edit.new_text);
    Ok(Some(AppliedEdit {
        edit,
        config_file,
        backup,
        change,
    }))
}

fn report(applied: &AppliedEdit, summary: String) -> String {
    format!(
        "{summary} in {}. Umbriel applies it as soon as the file is saved.\nBackup of the previous \
         config: {}\nChange:\n{}",
        applied.config_file.display(),
        applied.backup.display(),
        applied.change
    )
}

pub fn set_keybind(chord: &str, action: &str, repeat: Option<bool>) -> Result<String> {
    let Some(applied) = apply(chord, |config| {
        plan_set_keybind(config, chord, action, repeat)
    })?
    else {
        return Ok(format!(
            "{chord} is already bound to {}; nothing changed",
            action.trim()
        ));
    };
    let replaced = match &applied.edit.previous {
        Some(previous) => format!(" (was {previous})"),
        None => String::new(),
    };
    let summary = format!(
        "Bound {} to {}{replaced}",
        applied.edit.chord,
        action.trim()
    );
    Ok(report(&applied, summary))
}

pub fn remove_keybind(chord: &str) -> Result<String> {
    let applied = apply(chord, |config| plan_remove_keybind(config, chord))?.ok_or_else(|| {
        anyhow!("{chord} is not bound in the Umbriel config, so nothing was removed")
    })?;
    let was = applied
        .edit
        .previous
        .as_deref()
        .map(|previous| format!(" (was {previous})"))
        .unwrap_or_default();
    let summary = format!("Removed the binding for {}{was}", applied.edit.chord);
    Ok(report(&applied, summary))
}

#[cfg(test)]
mod tests {
    use super::{changed_lines, included_files, plan_remove_keybind, plan_set_keybind, same_chord};

    const CONFIG: &str = r#"# My Umbriel config
[general]
autostart = ["noctalia"]

[keybinds]
# launchers
"Mod+T" = { action = "spawn:kitty", repeat = false }
"Mod+Q" = "window-close"   # close
"Mod+Shift+Escape" = { action = "shortcuts-inhibit-toggle", allow_when_inhibited = true }

[layout]
gap = 8
"#;

    #[test]
    fn chords_match_regardless_of_modifier_order_and_case() {
        assert!(same_chord("Mod+Shift+T", "shift+mod+t"));
        assert!(same_chord("Mod+Return", "Mod + Return"));
        assert!(!same_chord("Mod+T", "Mod+Shift+T"));
        assert!(!same_chord("Mod+T", "Mod+Y"));
    }

    #[test]
    fn a_new_keybind_is_added_inside_the_keybinds_table_and_nothing_else_changes() {
        let edit = plan_set_keybind(CONFIG, "Mod+F", "window-toggle-fullscreen", None).unwrap();

        assert_eq!(edit.previous, None);
        assert_eq!(edit.chord, "Mod+F");
        assert_eq!(
            changed_lines(CONFIG, &edit.new_text),
            "+ \"Mod+F\" = \"window-toggle-fullscreen\""
        );
        let keybinds_at = edit.new_text.find("[keybinds]").unwrap();
        let layout_at = edit.new_text.find("[layout]").unwrap();
        let added_at = edit.new_text.find("\"Mod+F\"").unwrap();
        assert!(keybinds_at < added_at && added_at < layout_at);
        assert!(edit.new_text.contains("# launchers"));
        assert!(edit.new_text.contains("# close"));
    }

    #[test]
    fn a_repeat_setting_writes_the_table_form() {
        let edit =
            plan_set_keybind(CONFIG, "Mod+F", "window-toggle-fullscreen", Some(false)).unwrap();

        assert!(
            edit.new_text
                .contains(r#""Mod+F" = { action = "window-toggle-fullscreen", repeat = false }"#),
            "{}",
            edit.new_text
        );
    }

    #[test]
    fn rebinding_an_existing_chord_replaces_it_in_place_and_reports_the_old_binding() {
        let edit = plan_set_keybind(CONFIG, "mod+q", "window-kill", None).unwrap();

        assert_eq!(edit.chord, "Mod+Q");
        assert_eq!(edit.previous.as_deref(), Some("window-close"));
        assert_eq!(edit.new_text.matches("Mod+Q").count(), 1);
        assert_eq!(
            changed_lines(CONFIG, &edit.new_text),
            "- \"Mod+Q\" = \"window-close\"   # close\n+ \"Mod+Q\" = \"window-kill\"   # close"
        );
    }

    #[test]
    fn rebinding_a_table_entry_keeps_its_other_settings() {
        let edit = plan_set_keybind(CONFIG, "Mod+Shift+Escape", "session-quit", None).unwrap();

        assert_eq!(edit.previous.as_deref(), Some("shortcuts-inhibit-toggle"));
        assert!(
            edit.new_text.contains(
                r#""Mod+Shift+Escape" = { action = "session-quit", allow_when_inhibited = true }"#
            ),
            "{}",
            edit.new_text
        );
    }

    #[test]
    fn a_config_without_keybinds_gets_a_keybinds_table() {
        let edit = plan_set_keybind("[layout]\ngap = 8\n", "Mod+T", "spawn:foot", None).unwrap();

        assert!(
            edit.new_text
                .contains("[keybinds]\n\"Mod+T\" = \"spawn:foot\""),
            "{}",
            edit.new_text
        );
        assert!(edit.new_text.starts_with("[layout]\ngap = 8\n"));
    }

    #[test]
    fn removing_a_keybind_drops_only_that_line() {
        let edit = plan_remove_keybind(CONFIG, "Mod+T").unwrap();

        assert_eq!(edit.previous.as_deref(), Some("spawn:kitty"));
        assert_eq!(
            changed_lines(CONFIG, &edit.new_text),
            "- \"Mod+T\" = { action = \"spawn:kitty\", repeat = false }"
        );
    }

    #[test]
    fn removing_an_unbound_chord_is_an_error_naming_it() {
        let error = plan_remove_keybind(CONFIG, "Mod+Z")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Mod+Z") && error.contains("not bound"),
            "{error}"
        );
    }

    #[test]
    fn malformed_input_is_refused() {
        assert!(plan_set_keybind("[keybinds\n", "Mod+T", "spawn:x", None).is_err());
        assert!(plan_set_keybind(CONFIG, "  ", "window-close", None).is_err());
        assert!(plan_set_keybind(CONFIG, "Mod+T", " ", None).is_err());
        assert!(plan_set_keybind("keybinds = 3\n", "Mod+T", "window-close", None).is_err());
    }

    #[test]
    fn included_files_are_listed_from_both_include_tables() {
        let config =
            "[include]\nfiles = [\"binds.toml\"]\n\n[include.optional]\nfiles = [\"local.toml\"]\n";
        assert_eq!(included_files(config), vec!["binds.toml", "local.toml"]);
        assert!(included_files(CONFIG).is_empty());
    }
}
