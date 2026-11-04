use crate::config_edit::{PreparedChange, load_user_config, prepare};
use crate::setting::toml_value;
use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value as Json};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};

const RULE_LIST_TYPE: &str = "array_of_tables";
const MATCH_FIELD: &str = "match";
const SECTION_TYPES: [&str; 2] = ["table", "map"];

#[derive(Debug, PartialEq, Eq)]
pub struct PlannedRule {
    pub new_text: String,
    pub summary: String,
}

fn path_of(option: &Json) -> &str {
    option
        .get("path")
        .and_then(Json::as_str)
        .unwrap_or_default()
}

fn type_of(option: &Json) -> &str {
    option
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or_default()
}

pub fn rule_lists(options: &[Json]) -> Vec<String> {
    options
        .iter()
        .filter(|option| type_of(option) == RULE_LIST_TYPE && !path_of(option).contains("[]"))
        .map(|option| path_of(option).to_owned())
        .collect()
}

fn flatten_into(prefix: &str, object: &Map<String, Json>, fields: &mut Vec<(String, Json)>) {
    for (key, value) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Json::Object(nested) if !nested.is_empty() => flatten_into(&path, nested, fields),
            other => fields.push((path, other.clone())),
        }
    }
}

pub fn flatten_rule(rule: &Json) -> Result<Vec<(String, Json)>> {
    let object = rule
        .as_object()
        .ok_or_else(|| anyhow!("a rule must be a JSON object of field names and values"))?;
    let mut fields = Vec::new();
    flatten_into("", object, &mut fields);
    if fields.is_empty() {
        return Err(anyhow!("a rule needs at least one field"));
    }
    fields.sort_by_key(|(path, _)| path.split('.').next() != Some(MATCH_FIELD));
    Ok(fields)
}

pub fn checked_fields(options: &[Json], list: &str, rule: &Json) -> Result<Vec<(String, Value)>> {
    let lists = rule_lists(options);
    if !lists.iter().any(|known| known == list) {
        return Err(anyhow!(
            "`{list}` is not a rule list; the rule lists are: {}",
            lists.join(", ")
        ));
    }
    flatten_rule(rule)?
        .into_iter()
        .map(|(field, value)| {
            let wanted = format!("{list}[].{field}");
            let option = options
                .iter()
                .find(|option| path_of(option) == wanted)
                .ok_or_else(|| {
                    anyhow!(
                        "`{field}` is not a {list} field; config_options with query \"{list}\" \
                         lists the fields"
                    )
                })?;
            if SECTION_TYPES.contains(&type_of(option)) {
                return Err(anyhow!(
                    "`{list}.{field}` is a group of fields; give the fields inside it, for example \
                     {{\"{field}\": {{\"<field>\": <value>}}}}"
                ));
            }
            Ok((field, toml_value(option, &value)?))
        })
        .collect()
}

fn parse(config: &str) -> Result<DocumentMut> {
    config
        .parse::<DocumentMut>()
        .context("the Umbriel config is not valid TOML, so it was not changed")
}

fn split_list(list: &str) -> (Vec<&str>, &str) {
    let mut segments: Vec<&str> = list.split('.').collect();
    let leaf = segments.pop().unwrap_or_default();
    (segments, leaf)
}

fn existing_entries<'a>(
    document: &'a mut DocumentMut,
    list: &str,
) -> Option<&'a mut ArrayOfTables> {
    let (sections, leaf) = split_list(list);
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for section in sections {
        table = table.get_mut(section)?.as_table_like_mut()?;
    }
    table.get_mut(leaf)?.as_array_of_tables_mut()
}

fn entries_for_adding<'a>(
    document: &'a mut DocumentMut,
    list: &str,
) -> Result<&'a mut ArrayOfTables> {
    let (sections, leaf) = split_list(list);
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for section in sections {
        if table.get(section).is_none() {
            let mut created = Table::new();
            created.set_implicit(true);
            table.insert(section, Item::Table(created));
        }
        table = table
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                anyhow!("`{section}` in your config is not a section, so it was not changed")
            })?;
    }
    if table.get(leaf).is_none() {
        table.insert(leaf, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    table
        .get_mut(leaf)
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| anyhow!("`{list}` in your config is not a list of [[{list}]] entries"))
}

fn entry_table(fields: Vec<(String, Value)>) -> Table {
    let mut entry = Table::new();
    entry.decor_mut().set_prefix("\n");
    for (path, value) in fields {
        let mut segments: Vec<&str> = path.split('.').collect();
        let leaf = segments.pop().unwrap_or_default();
        let mut current = &mut entry;
        for segment in segments {
            if !current.contains_key(segment) {
                let mut nested = Table::new();
                nested.set_dotted(true);
                current.insert(segment, Item::Table(nested));
            }
            current = current[segment]
                .as_table_mut()
                .expect("a rule field path only nests tables");
        }
        current.insert(leaf, Item::Value(value));
    }
    entry
}

fn describe_into(prefix: &str, table: &dyn TableLike, parts: &mut Vec<String>) {
    for (key, item) in table.iter() {
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(nested) = item.as_table_like() {
            describe_into(&path, nested, parts);
        } else if let Some(value) = item.as_value() {
            let mut plain = value.clone();
            plain.decor_mut().clear();
            parts.push(format!("{path} = {plain}"));
        }
    }
}

fn describe(table: &Table) -> String {
    let mut parts = Vec::new();
    describe_into("", table, &mut parts);
    parts.join(", ")
}

fn entry_index(entries: Option<&ArrayOfTables>, list: &str, position: usize) -> Result<usize> {
    let count = entries.map(ArrayOfTables::len).unwrap_or_default();
    if count == 0 {
        return Err(anyhow!(
            "your config has no {list} entries; read_config shows what is there"
        ));
    }
    if position == 0 || position > count {
        return Err(anyhow!(
            "your config has {count} {list} entries, so position {position} does not exist; \
             positions count from 1 in the order read_config shows them"
        ));
    }
    Ok(position - 1)
}

pub fn plan_add(config: &str, list: &str, fields: Vec<(String, Value)>) -> Result<PlannedRule> {
    let mut document = parse(config)?;
    let entries = entries_for_adding(&mut document, list)?;
    let entry = entry_table(fields);
    let summary = format!("Add {list} #{}: {}", entries.len() + 1, describe(&entry));
    entries.push(entry);
    Ok(PlannedRule {
        new_text: document.to_string(),
        summary,
    })
}

pub fn plan_replace(
    config: &str,
    list: &str,
    position: usize,
    fields: Vec<(String, Value)>,
) -> Result<PlannedRule> {
    let mut document = parse(config)?;
    let entries = existing_entries(&mut document, list);
    let index = entry_index(entries.as_deref(), list, position)?;
    let slot = entries
        .and_then(|entries| entries.get_mut(index))
        .expect("index checked above");
    let mut replacement = entry_table(fields);
    *replacement.decor_mut() = slot.decor().clone();
    if let Some(position_in_file) = slot.position() {
        replacement.set_position(Some(position_in_file));
    }
    let summary = format!(
        "Replace {list} #{position} with: {} (was: {})",
        describe(&replacement),
        describe(slot)
    );
    *slot = replacement;
    Ok(PlannedRule {
        new_text: document.to_string(),
        summary,
    })
}

pub fn plan_remove(config: &str, list: &str, position: usize) -> Result<PlannedRule> {
    let mut document = parse(config)?;
    let entries = existing_entries(&mut document, list);
    let index = entry_index(entries.as_deref(), list, position)?;
    let entries = entries.expect("index checked above");
    let summary = format!(
        "Remove {list} #{position} ({})",
        describe(entries.get(index).expect("index checked above"))
    );
    entries.remove(index);
    Ok(PlannedRule {
        new_text: document.to_string(),
        summary,
    })
}

const KEYBOARD_DEVICE_FIELDS: [&str; 4] = ["layout", "variant", "model", "options"];

pub fn needs_full_profile(list: &str, fields: &[(String, Value)]) -> Option<&'static str> {
    if list == "security_context_rule" {
        return Some("security context rules decide which protocols sandboxed apps may use");
    }
    let spawns = list == "scratchpad"
        && fields.iter().any(|(field, value)| {
            field == "spawn_when_empty"
                && value
                    .as_str()
                    .is_some_and(|command| !command.trim().is_empty())
        });
    spawns.then_some("a scratchpad's spawn_when_empty starts a program")
}

fn caution_for(list: &str, fields: &[(String, Value)]) -> Option<String> {
    if list != "input.device" {
        return None;
    }
    let disables = fields
        .iter()
        .any(|(field, value)| field == "enabled" && value.as_bool() == Some(false));
    let rekeys = fields
        .iter()
        .any(|(field, _)| KEYBOARD_DEVICE_FIELDS.contains(&field.as_str()));
    match (disables, rekeys) {
        (true, _) => Some(
            "this disables a device; if it is your keyboard or mouse, restore the backup from a TTY"
                .to_owned(),
        ),
        (false, true) => Some(
            "a wrong keyboard setting can stop you typing; if that happens, restore the backup from a TTY"
                .to_owned(),
        ),
        _ => None,
    }
}

pub fn fields_for(list: &str, rule: &Json) -> Result<Vec<(String, Value)>> {
    checked_fields(&crate::compositor::schema_options()?, list.trim(), rule)
}

pub fn prepare_add(list: &str, fields: Vec<(String, Value)>) -> Result<Option<PreparedChange>> {
    let caution = caution_for(list, &fields);
    let (config_file, original) = load_user_config()?;
    let planned = plan_add(&original, list, fields)?;
    prepare(
        config_file,
        original,
        planned.new_text,
        planned.summary,
        caution,
    )
}

pub fn prepare_replace(
    list: &str,
    position: usize,
    fields: Vec<(String, Value)>,
) -> Result<Option<PreparedChange>> {
    let caution = caution_for(list, &fields);
    let (config_file, original) = load_user_config()?;
    let planned = plan_replace(&original, list, position, fields)?;
    prepare(
        config_file,
        original,
        planned.new_text,
        planned.summary,
        caution,
    )
}

pub fn prepare_remove(list: &str, position: usize) -> Result<PreparedChange> {
    let (config_file, original) = load_user_config()?;
    let planned = plan_remove(&original, list.trim(), position)?;
    prepare(
        config_file,
        original,
        planned.new_text,
        planned.summary,
        None,
    )?
    .ok_or_else(|| anyhow!("removing {list} #{position} would not change the config"))
}

#[cfg(test)]
mod tests {
    use super::{checked_fields, flatten_rule, plan_add, plan_remove, plan_replace, rule_lists};
    use serde_json::{Value as Json, json};
    use toml_edit::Value;

    fn options() -> Vec<Json> {
        json!([
            {"path": "window_rule", "type": "array_of_tables"},
            {"path": "window_rule[].match", "type": "table"},
            {"path": "window_rule[].match.app_id", "type": "string_or_string_array", "format": "regex"},
            {"path": "window_rule[].default_floating", "type": "bool"},
            {"path": "window_rule[].opacity", "type": "float", "min": 0.0, "max": 1.0},
            {"path": "window_rule[].default_floating_size_px", "type": "table"},
            {"path": "window_rule[].default_floating_size_px.width", "type": "int", "min": 1, "max": 100000},
            {"path": "input.device", "type": "array_of_tables"},
            {"path": "input.device[].name", "type": "string"},
            {"path": "input.device[].natural_scroll", "type": "bool"},
            {"path": "layout.gap", "type": "int"}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    fn fields(rule: Json) -> Vec<(String, Value)> {
        checked_fields(&options(), "window_rule", &rule).unwrap()
    }

    const CONFIG: &str = "[general]\nxwayland = true\n\n# floating tools\n[[window_rule]]\nmatch.app_id = \"^pavucontrol$\"\ndefault_floating = true\n\n[[window_rule]]\nblur = true\n\n[layout]\ngap = 8\n";

    #[test]
    fn rule_lists_come_from_the_schema() {
        assert_eq!(rule_lists(&options()), ["window_rule", "input.device"]);
    }

    #[test]
    fn nested_and_dotted_rules_flatten_the_same_way() {
        let nested =
            flatten_rule(&json!({"match": {"app_id": "^mpv$"}, "default_floating": true})).unwrap();
        let dotted =
            flatten_rule(&json!({"match.app_id": "^mpv$", "default_floating": true})).unwrap();

        assert_eq!(nested, dotted);
        assert!(nested.contains(&("match.app_id".to_owned(), json!("^mpv$"))));
        assert!(flatten_rule(&json!({})).is_err());
        assert!(flatten_rule(&json!(["not", "an", "object"])).is_err());
    }

    #[test]
    fn rule_fields_are_checked_against_the_schema() {
        let error = |rule: Json| {
            checked_fields(&options(), "window_rule", &rule)
                .unwrap_err()
                .to_string()
        };

        assert!(error(json!({"opacity": 2.0})).contains("must be between"));
        assert!(error(json!({"floating": true})).contains("not a window_rule field"));
        assert!(error(json!({"match": "mpv"})).contains("window_rule.match"));
        assert!(
            checked_fields(&options(), "layout", &json!({"gap": 1}))
                .unwrap_err()
                .to_string()
                .contains("not a rule list")
        );
    }

    #[test]
    fn a_new_rule_is_appended_after_the_existing_ones_and_nothing_else_changes() {
        let planned = plan_add(
            CONFIG,
            "window_rule",
            fields(json!({"match": {"app_id": "^mpv$"}, "default_floating": true})),
        )
        .unwrap();

        assert!(
            planned.summary.starts_with("Add window_rule #3"),
            "{}",
            planned.summary
        );
        assert!(
            planned.summary.contains("match.app_id = \"^mpv$\""),
            "{}",
            planned.summary
        );
        let text = &planned.new_text;
        assert!(
            text.contains("[[window_rule]]\nmatch.app_id = \"^mpv$\"\ndefault_floating = true"),
            "{text}"
        );
        assert_eq!(text.matches("[[window_rule]]").count(), 3, "{text}");
        assert!(
            text.starts_with("[general]\nxwayland = true\n\n# floating tools\n[[window_rule]]"),
            "{text}"
        );
        assert!(text.contains("[layout]\ngap = 8"), "{text}");
    }

    #[test]
    fn nested_tables_inside_a_rule_are_written_as_dotted_keys() {
        let planned = plan_add(
            CONFIG,
            "window_rule",
            fields(json!({"match.app_id": "^x$", "default_floating_size_px": {"width": 800}})),
        )
        .unwrap();

        assert!(
            planned
                .new_text
                .contains("default_floating_size_px.width = 800"),
            "{}",
            planned.new_text
        );
    }

    #[test]
    fn a_rule_list_under_a_section_is_created_where_it_belongs() {
        let device = checked_fields(
            &options(),
            "input.device",
            &json!({"name": "touchpad", "natural_scroll": true}),
        )
        .unwrap();
        let planned = plan_add(CONFIG, "input.device", device).unwrap();

        assert!(
            planned
                .new_text
                .contains("[[input.device]]\nname = \"touchpad\"\nnatural_scroll = true"),
            "{}",
            planned.new_text
        );
    }

    #[test]
    fn replacing_a_rule_keeps_its_position_and_the_comment_above_it() {
        let planned = plan_replace(
            CONFIG,
            "window_rule",
            1,
            fields(json!({"match": {"app_id": "^pavucontrol$"}, "opacity": 0.9})),
        )
        .unwrap();

        assert!(
            planned.summary.contains("Replace window_rule #1"),
            "{}",
            planned.summary
        );
        assert!(
            planned
                .summary
                .contains("was: match.app_id = \"^pavucontrol$\", default_floating = true"),
            "{}",
            planned.summary
        );
        assert!(
            planned.new_text.contains(
                "# floating tools\n[[window_rule]]\nmatch.app_id = \"^pavucontrol$\"\nopacity = 0.9"
            ),
            "{}",
            planned.new_text
        );
        assert!(planned.new_text.contains("blur = true"));
    }

    #[test]
    fn removing_a_rule_drops_only_that_entry() {
        let planned = plan_remove(CONFIG, "window_rule", 2).unwrap();

        assert!(
            planned
                .summary
                .contains("Remove window_rule #2 (blur = true)"),
            "{}",
            planned.summary
        );
        assert!(
            !planned.new_text.contains("blur = true"),
            "{}",
            planned.new_text
        );
        assert_eq!(planned.new_text.matches("[[window_rule]]").count(), 1);
        assert!(planned.new_text.contains("default_floating = true"));
    }

    #[test]
    fn positions_outside_the_list_are_refused_with_the_count() {
        for position in [0, 3] {
            let error = plan_remove(CONFIG, "window_rule", position)
                .unwrap_err()
                .to_string();
            assert!(error.contains("has 2 window_rule entries"), "{error}");
        }
        let error = plan_remove(CONFIG, "layer_rule", 1)
            .unwrap_err()
            .to_string();
        assert!(error.contains("has no layer_rule entries"), "{error}");
    }
}
