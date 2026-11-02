use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::{Path, PathBuf};

const WHOLE_SCREEN: &str = "screen";
const OUTPUT_PREFIX: &str = "output:";
const WINDOW_PREFIX: &str = "window:";
const LONGEST_WINDOW_TITLE: usize = 48;
const RECORDINGS_FOLDER: &str = "umbriel-mcp";
const SECONDS_PER_DAY: u64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Screenshot,
    Recording,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    WholeScreen,
    Output(String),
    Window(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub title: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordingArea {
    Output(String),
    Region { geometry: String },
}

fn text<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn size_of(output: &Value) -> String {
    let dimension = |key: &str| {
        output
            .get("logical_size")
            .and_then(|size| size.get(key))
            .and_then(Value::as_i64)
    };
    match (dimension("width"), dimension("height")) {
        (Some(width), Some(height)) => format!(" ({width}x{height})"),
        _ => String::new(),
    }
}

fn shortened(title: &str) -> String {
    if title.chars().count() <= LONGEST_WINDOW_TITLE {
        return title.to_owned();
    }
    let kept: String = title.chars().take(LONGEST_WINDOW_TITLE - 1).collect();
    format!("{}…", kept.trim_end())
}

fn window_title(window: &Value) -> String {
    let title = match text(window, "title") {
        "" => text(window, "app_id"),
        title => title,
    };
    let hidden = if window.get("visible").and_then(Value::as_bool) == Some(false) {
        ", not shown now"
    } else {
        ""
    };
    format!(
        "Window: {} ({}, workspace {}{hidden})",
        shortened(title),
        text(window, "app_id"),
        text(window, "workspace")
    )
}

pub fn capture_choices(outputs: &[Value], windows: &[Value], purpose: Purpose) -> Vec<Choice> {
    let mut choices = Vec::new();
    if purpose == Purpose::Screenshot {
        choices.push(Choice {
            value: WHOLE_SCREEN.to_owned(),
            title: "Whole screen (all monitors)".to_owned(),
        });
    }
    let monitor_label = match purpose {
        Purpose::Screenshot => "Monitor",
        Purpose::Recording => "Whole monitor",
    };
    for output in outputs
        .iter()
        .filter(|output| output.get("enabled").and_then(Value::as_bool) != Some(false))
    {
        let name = text(output, "name");
        if name.is_empty() {
            continue;
        }
        choices.push(Choice {
            value: format!("{OUTPUT_PREFIX}{name}"),
            title: format!("{monitor_label} {name}{}", size_of(output)),
        });
    }
    for window in windows {
        let id = text(window, "id");
        let shown = window.get("visible").and_then(Value::as_bool) != Some(false);
        if id.is_empty() || (purpose == Purpose::Recording && !shown) {
            continue;
        }
        choices.push(Choice {
            value: format!("{WINDOW_PREFIX}{id}"),
            title: window_title(window),
        });
    }
    choices
}

pub fn parse_choice(value: &str) -> Result<CaptureTarget> {
    let value = value.trim();
    let non_empty = |rest: &str| (!rest.is_empty()).then(|| rest.to_owned());
    if value == WHOLE_SCREEN {
        return Ok(CaptureTarget::WholeScreen);
    }
    if let Some(name) = value.strip_prefix(OUTPUT_PREFIX).and_then(non_empty) {
        return Ok(CaptureTarget::Output(name));
    }
    if let Some(id) = value.strip_prefix(WINDOW_PREFIX).and_then(non_empty) {
        return Ok(CaptureTarget::Window(id));
    }
    Err(anyhow!(
        "{value:?} is not one of the offered capture choices"
    ))
}

pub fn choices_for_the_model(choices: &[Choice]) -> String {
    choices
        .iter()
        .map(|choice| {
            let arguments = match parse_choice(&choice.value) {
                Ok(CaptureTarget::WholeScreen) => "leave output and window_id empty".to_owned(),
                Ok(CaptureTarget::Output(name)) => format!("output \"{name}\""),
                Ok(CaptureTarget::Window(id)) => format!("window_id \"{id}\""),
                Err(_) => String::new(),
            };
            format!("- {}: {arguments}", choice.title)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn recorder_arguments(file: &Path, area: &RecordingArea, audio: bool) -> Vec<String> {
    let mut arguments = vec!["-y".to_owned(), "-f".to_owned(), file.display().to_string()];
    match area {
        RecordingArea::Output(name) => arguments.extend(["-o".to_owned(), name.clone()]),
        RecordingArea::Region { geometry } => arguments.extend(["-g".to_owned(), geometry.clone()]),
    }
    if audio {
        arguments.push("-a".to_owned());
    }
    arguments
}

fn configured_videos_folder(user_dirs: &str, home: &str) -> Option<PathBuf> {
    let configured = user_dirs
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix("XDG_VIDEOS_DIR="))?
        .trim_matches('"');
    let expanded = configured.replace("$HOME", home);
    let folder = PathBuf::from(expanded.trim_end_matches('/'));
    (folder.is_absolute() && folder != Path::new(home)).then_some(folder)
}

pub fn videos_directory(user_dirs: Option<&str>, home: &str) -> PathBuf {
    user_dirs
        .and_then(|user_dirs| configured_videos_folder(user_dirs, home))
        .unwrap_or_else(|| PathBuf::from(home).join("Videos"))
        .join(RECORDINGS_FOLDER)
}

fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub const RECORDING_LENGTHS: [u32; 7] = [30, 60, 120, 300, 600, 1800, 3600];
pub const DEFAULT_RECORDING_SECONDS: u32 = 30;
pub const LONGEST_RECORDING_SECONDS: u32 = 3600;

fn counted(amount: u32, unit: &str) -> String {
    if amount == 1 {
        format!("1 {unit}")
    } else {
        format!("{amount} {unit}s")
    }
}

pub fn human_duration(seconds: u32) -> String {
    let (minutes, remainder) = (seconds / 60, seconds % 60);
    match (minutes, remainder) {
        (0, _) => counted(remainder, "second"),
        (_, 0) => counted(minutes, "minute"),
        _ => format!(
            "{} {}",
            counted(minutes, "minute"),
            counted(remainder, "second")
        ),
    }
}

pub fn utc_stamp(unix_seconds: u64) -> String {
    let (year, month, day) = civil_date((unix_seconds / SECONDS_PER_DAY) as i64);
    let seconds_today = unix_seconds % SECONDS_PER_DAY;
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        seconds_today / 3600,
        seconds_today % 3600 / 60,
        seconds_today % 60
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureTarget, Choice, DEFAULT_RECORDING_SECONDS, Purpose, RECORDING_LENGTHS,
        RecordingArea, capture_choices, choices_for_the_model, human_duration, parse_choice,
        recorder_arguments, utc_stamp, videos_directory,
    };
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};

    fn outputs() -> Vec<Value> {
        json!([
            {"name": "HDMI-A-1", "enabled": true, "logical_size": {"width": 1920, "height": 1080}},
            {"name": "DP-1", "enabled": true, "logical_size": {"width": 2560, "height": 1440}},
            {"name": "eDP-1", "enabled": false, "logical_size": {"width": 1920, "height": 1200}}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    fn windows() -> Vec<Value> {
        json!([
            {"id": "aaa", "title": "Figma", "app_id": "figma-linux-next", "workspace": "HDMI-A-1:4", "visible": false},
            {"id": "bbb", "title": "~/Projects/umbriel-mcp — a very long terminal title that keeps going on", "app_id": "kitty", "workspace": "DP-1:1", "visible": true}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    fn values(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.value.as_str()).collect()
    }

    #[test]
    fn screenshots_offer_the_whole_screen_each_enabled_monitor_and_every_window() {
        let choices = capture_choices(&outputs(), &windows(), Purpose::Screenshot);

        assert_eq!(
            values(&choices),
            [
                "screen",
                "output:HDMI-A-1",
                "output:DP-1",
                "window:aaa",
                "window:bbb"
            ]
        );
        assert_eq!(choices[0].title, "Whole screen (all monitors)");
        assert_eq!(choices[2].title, "Monitor DP-1 (2560x1440)");
        assert_eq!(
            choices[3].title,
            "Window: Figma (figma-linux-next, workspace HDMI-A-1:4, not shown now)"
        );
    }

    #[test]
    fn long_window_titles_are_shortened() {
        let choices = capture_choices(&outputs(), &windows(), Purpose::Screenshot);
        let terminal = &choices[4].title;

        assert!(terminal.chars().count() < 110, "{terminal}");
        assert!(terminal.contains('…'), "{terminal}");
        assert!(terminal.contains("(kitty, workspace DP-1:1)"), "{terminal}");
    }

    #[test]
    fn recordings_offer_monitors_and_only_windows_that_are_on_screen() {
        let choices = capture_choices(&outputs(), &windows(), Purpose::Recording);

        assert_eq!(
            values(&choices),
            ["output:HDMI-A-1", "output:DP-1", "window:bbb"]
        );
        assert_eq!(choices[0].title, "Whole monitor HDMI-A-1 (1920x1080)");
    }

    #[test]
    fn a_chosen_value_becomes_a_capture_target() {
        assert_eq!(parse_choice("screen").unwrap(), CaptureTarget::WholeScreen);
        assert_eq!(
            parse_choice("output:DP-1").unwrap(),
            CaptureTarget::Output("DP-1".into())
        );
        assert_eq!(
            parse_choice("window:bbb").unwrap(),
            CaptureTarget::Window("bbb".into())
        );
        assert!(parse_choice("elsewhere").is_err());
        assert!(parse_choice("output:").is_err());
    }

    #[test]
    fn the_model_gets_the_choices_with_the_arguments_for_each() {
        let choices = capture_choices(&outputs(), &windows(), Purpose::Screenshot);
        let text = choices_for_the_model(&choices);

        assert!(
            text.contains("- Whole screen (all monitors): leave output and window_id empty"),
            "{text}"
        );
        assert!(
            text.contains("- Monitor DP-1 (2560x1440): output \"DP-1\""),
            "{text}"
        );
        assert!(text.contains("window_id \"aaa\""), "{text}");
    }

    #[test]
    fn the_recorder_never_prompts_and_records_one_area() {
        let file = Path::new("/v/recording.mp4");

        assert_eq!(
            recorder_arguments(file, &RecordingArea::Output("DP-1".into()), false),
            ["-y", "-f", "/v/recording.mp4", "-o", "DP-1"]
        );
        assert_eq!(
            recorder_arguments(
                file,
                &RecordingArea::Region {
                    geometry: "10,20 800x600".into()
                },
                true
            ),
            ["-y", "-f", "/v/recording.mp4", "-g", "10,20 800x600", "-a"]
        );
    }

    #[test]
    fn recordings_go_to_the_xdg_videos_folder() {
        let user_dirs =
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_VIDEOS_DIR=\"$HOME/Clips\"\n";
        assert_eq!(
            videos_directory(Some(user_dirs), "/home/u"),
            PathBuf::from("/home/u/Clips/umbriel-mcp")
        );
        assert_eq!(
            videos_directory(Some("XDG_VIDEOS_DIR=\"/data/videos\"\n"), "/home/u"),
            PathBuf::from("/data/videos/umbriel-mcp")
        );
        assert_eq!(
            videos_directory(None, "/home/u"),
            PathBuf::from("/home/u/Videos/umbriel-mcp")
        );
        assert_eq!(
            videos_directory(Some("XDG_VIDEOS_DIR=\"$HOME/\"\n"), "/home/u"),
            PathBuf::from("/home/u/Videos/umbriel-mcp"),
            "a videos folder of $HOME itself means none is set"
        );
    }

    #[test]
    fn file_names_use_a_sortable_utc_timestamp() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_793_580_000), "20261102-004000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(1), "1 second");
        assert_eq!(human_duration(30), "30 seconds");
        assert_eq!(human_duration(60), "1 minute");
        assert_eq!(human_duration(90), "1 minute 30 seconds");
        assert_eq!(human_duration(600), "10 minutes");
        assert_eq!(human_duration(3600), "60 minutes");
    }

    #[test]
    fn the_offered_lengths_start_at_the_default() {
        assert_eq!(RECORDING_LENGTHS[0], DEFAULT_RECORDING_SECONDS);
        assert_eq!(DEFAULT_RECORDING_SECONDS, 30);
    }
}
