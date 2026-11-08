mod actions;
mod approval;
mod capture_target;
mod compositor;
mod config_edit;
mod doctor;
mod input;
mod ipc;
mod keyboard;
mod listing;
mod placement;
mod profile;
mod recording;
mod register;
mod rules;
mod screen;
mod setting;
mod uninstall;
mod update;
mod watching;

use base64::prelude::{BASE64_STANDARD, Engine};
use capture_target::{CaptureTarget, Purpose};
use clap::Parser;
use profile::Profile;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{Peer, RoleServer};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use std::sync::Arc;

fn failed(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

fn as_pretty_json(value: serde_json::Value) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(&value).map_err(|cause| failed(cause.into()))
}

const DEFAULT_KEPT_BACKUPS: u32 = 5;
const FOCUS_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
const FOCUS_POLL: std::time::Duration = std::time::Duration::from_millis(25);

fn focused_window_id() -> Option<String> {
    ipc::focused_window()
        .ok()?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

fn wait_for_keyboard_focus(window_id: &str) -> anyhow::Result<String> {
    let deadline = std::time::Instant::now() + FOCUS_WAIT;
    loop {
        let focused = focused_window_id();
        if focused.as_deref() == Some(window_id) {
            return Ok(format!("ok: {window_id} now has keyboard focus"));
        }
        if std::time::Instant::now() >= deadline {
            let holder = focused
                .map(|id| format!("window {id} still has it"))
                .unwrap_or_else(|| "no window has it".to_owned());
            return Err(anyhow::anyhow!(
                "window {window_id} did not receive keyboard focus within {} seconds ({holder}). \
                 The screen may be locked, or a menu, dialog or panel may be holding focus; \
                 typing now would not reach {window_id}.",
                FOCUS_WAIT.as_secs()
            ));
        }
        std::thread::sleep(FOCUS_POLL);
    }
}

fn focus_report() -> String {
    match ipc::focused_window() {
        Ok(window)
            if window
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some() =>
        {
            let field = |key: &str| {
                window
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            format!(
                "Keyboard focus is now on window {} ({}: {:?}).",
                field("id"),
                field("app_id"),
                field("title")
            )
        }
        Ok(_) => "No window has keyboard focus now.".to_owned(),
        Err(error) => format!("Could not read which window has focus: {error:#}."),
    }
}

fn require_keyboard_focus(window_id: &str, nothing_done: &str) -> Result<(), ErrorData> {
    let focused = ipc::focused_window().map_err(failed)?;
    let holder = focused.get("id").and_then(serde_json::Value::as_str);
    if holder == Some(window_id) {
        return Ok(());
    }
    let holder = match holder {
        Some(id) => {
            let field = |key: &str| {
                focused
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            format!(
                "window {id} ({}: {:?}) has it",
                field("app_id"),
                field("title")
            )
        }
        None => "no window has it".to_owned(),
    };
    Err(ErrorData::invalid_params(
        format!(
            "window {window_id} does not have keyboard focus ({holder}), so {nothing_done}. Call \
             focus_window first. If focus moved without you, the user may be using the desktop: \
             ask them before taking over."
        ),
        None,
    ))
}

fn refuse_unreachable_point(
    window_id: &str,
    placement: &ipc::WindowPlacement,
    x: i64,
    y: i64,
) -> Result<(), ErrorData> {
    let refusal = |reason: String| Err(ErrorData::invalid_params(reason, None));
    if !placement.shown_on_screen {
        return refusal(format!(
            "window {window_id} is not on screen, so the pointer would land on whatever is shown \
             there; switch to its workspace or call focus_window first"
        ));
    }
    if placement
        .output_area
        .is_some_and(|area| !area.contains_point(x, y))
    {
        return refusal(format!(
            "screen point ({x}, {y}) of window {window_id} is scrolled out of view; call \
             focus_window to bring the window into view, then look again"
        ));
    }
    if let Some(cover) = placement
        .drawn_over
        .iter()
        .find(|cover| cover.area.contains_point(x, y))
    {
        return refusal(format!(
            "screen point ({x}, {y}) of window {window_id} is covered by {}, so the click would \
             land on that instead",
            cover.description
        ));
    }
    Ok(())
}

fn point_in_window(window_id: &str, x: i32, y: i32) -> Result<(i32, i32), ErrorData> {
    let placement = ipc::window_placement(window_id).map_err(failed)?;
    let (width, height) = placement.size;
    if x < 0 || y < 0 || i64::from(x) >= width || i64::from(y) >= height {
        return Err(ErrorData::invalid_params(
            format!(
                "({x}, {y}) is outside window {window_id}, which is {width}x{height}; x and y \
                 count from its top-left corner"
            ),
            None,
        ));
    }
    let screen_x = placement.origin.0 + i64::from(x);
    let screen_y = placement.origin.1 + i64::from(y);
    refuse_unreachable_point(window_id, &placement, screen_x, screen_y)?;
    Ok((screen_x as i32, screen_y as i32))
}

fn pointer_target(
    window_id: Option<&str>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<Option<(i32, i32)>, ErrorData> {
    match (window_id, x, y) {
        (None, None, None) => Ok(None),
        (Some(id), Some(x), Some(y)) => point_in_window(id, x, y).map(Some),
        (None, Some(x), Some(y)) => Ok(Some((x, y))),
        _ => Err(ErrorData::invalid_params(
            "give both x and y (and window_id to make them relative to a window), or none of them \
             to click where the pointer is",
            None,
        )),
    }
}

fn move_pointer_to(x: i32, y: i32) -> Result<(), ErrorData> {
    input::move_pointer(x, y).map_err(|error| {
        failed(error.context(format!("could not move the pointer to screen ({x}, {y})")))
    })
}

fn crashed_tool_error(tool: &str, crash: tokio::task::JoinError) -> ErrorData {
    let reason = match crash.try_into_panic() {
        Ok(payload) => payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned()),
        Err(cancelled) => cancelled.to_string(),
    };
    ErrorData::internal_error(
        format!(
            "{tool} failed inside umbriel-mcp ({reason}). This is a bug; please report it at \
             https://github.com/dot-ASH/umbriel-mcp/issues. Other tools still work."
        ),
        None,
    )
}

fn setting_that_always_asks(key: &str, value: &serde_json::Value) -> Option<&'static str> {
    match setting::policy(key.trim()) {
        setting::Policy::RunsProgramsOrLoadsFiles => {
            Some("this setting can start programs or load files")
        }
        setting::Policy::WeakensSecurity => Some("this setting can weaken a security check"),
        setting::Policy::TakesAnAction if value.as_str().is_some_and(profile::is_spawn_action) => {
            Some("it starts a program")
        }
        _ => None,
    }
}

fn acted(action: actions::CompositorAction) -> Result<String, ErrorData> {
    ipc::perform(&action).map_err(failed)?;
    Ok(format!("ok: {}", action.summary()))
}

fn workspace_matches(window_workspace: &str, wanted: &str) -> bool {
    let wanted = wanted.trim();
    window_workspace == wanted
        || window_workspace
            .rsplit_once(':')
            .is_some_and(|(_, number_or_name)| number_or_name == wanted)
}

fn refused(reason: String) -> ErrorData {
    ErrorData::invalid_request(reason, None)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WindowFilter {
    /// Also include every field Umbriel reports (effects, tabs, scratchpad and so on); omit for
    /// the brief form
    detail: Option<bool>,
    /// Workspace to list: a number or name such as "4" (that workspace on every monitor),
    /// or a full id such as "HDMI-A-1:4"
    workspace: Option<String>,
    /// Only list windows on this output, such as "DP-1"
    output: Option<String>,
    /// Only list windows of this application id, such as "firefox" (case does not matter)
    app_id: Option<String>,
    /// Only list windows owned by this process id
    pid: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DetailRequest {
    /// Also include every field Umbriel reports; omit for the brief form
    detail: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WorkspaceTarget {
    /// Workspace position (1-64) or exact name
    workspace: String,
    /// Restrict the selection to this output, such as "HDMI-A-1"
    output: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WindowTarget {
    /// Window id from list_windows; omit to act on the focused window
    window_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FocusTarget {
    /// Window id from list_windows
    window_id: String,
    /// Also warp the pointer onto the window
    warp_cursor: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ResizeRequest {
    /// Which edge to move: left, right, up or down
    edge: String,
    /// Signed fraction between -0.9 and 0.9
    delta: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawAction {
    /// An action with its arguments as list_actions shows them, such as "overview-toggle" or
    /// "workspace-switch 2 --output DP-1"; the older "workspace-switch:2/DP-1" form also works
    action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SpawnRequest {
    /// Command line to run
    command: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PointerPosition {
    /// Absolute screen x, or relative to the window's top-left when window_id is given
    x: i32,
    /// Absolute screen y, or relative to the window's top-left when window_id is given
    y: i32,
    /// Make x and y relative to this window; refused if it is not on screen
    window_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClickRequest {
    /// left, right, middle or side
    button: Option<String>,
    /// Issue a double click
    double: Option<bool>,
    /// Click at this x instead of where the pointer is: absolute, or relative to window_id
    x: Option<i32>,
    /// Click at this y instead of where the pointer is: absolute, or relative to window_id
    y: Option<i32>,
    /// Click inside this window at x, y relative to its top-left; refused if it is not on screen
    window_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScrollRequest {
    /// up or down
    direction: Option<String>,
    /// Number of wheel steps
    amount: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DragRequest {
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    /// left, right, middle or side
    button: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TypeRequest {
    /// Literal text to type into the focused surface
    text: String,
    /// Only type if this window has keyboard focus; otherwise nothing is typed
    window_id: Option<String>,
    /// Delay between keystrokes in milliseconds
    delay_ms: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeyRequest {
    /// Chord such as "ctrl+c", "alt+Tab" or "Return"
    keys: String,
    /// Only press if this window has keyboard focus; otherwise nothing is pressed
    window_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
struct RegionRequest {
    /// Left edge in absolute screen coordinates
    x: i64,
    /// Top edge in absolute screen coordinates
    y: i64,
    width: u32,
    height: u32,
}

impl From<RegionRequest> for screen::Region {
    fn from(region: RegionRequest) -> Self {
        Self {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        }
    }
}

fn area_from(
    output: Option<String>,
    window_id: Option<String>,
    region: Option<RegionRequest>,
) -> Result<screen::Area, ErrorData> {
    screen::Area::from_choice(output, window_id, region.map(Into::into))
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
}

fn capture_description(shot: &screen::Capture, notes: &[String]) -> String {
    std::iter::once(shot.coordinate_note())
        .chain(notes.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn matches_described(shot: &screen::Capture, matches: &[screen::TextMatch]) -> String {
    let mut lines: Vec<String> = matches
        .iter()
        .map(|hit| {
            let (x, y) = shot.screen_point(hit.center_x, hit.center_y);
            format!(
                "{:?} confidence {:.0} at screen ({x}, {y})",
                hit.text, hit.confidence
            )
        })
        .collect();
    if !shot.shown_on_screen {
        lines.push(
            "The window is not currently shown, so these coordinates only apply after switching \
             to its workspace."
                .to_owned(),
        );
    }
    lines.join("\n")
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CaptureRequest {
    /// Capture only this output, such as "HDMI-A-1"
    output: Option<String>,
    /// Capture only this window id, even if it is on a workspace that is not shown. Prefer this
    /// over whole-monitor captures: it is sharper and leaves out the user's other windows
    window_id: Option<String>,
    /// Capture only this rectangle of the screen, to zoom in on a detail at full resolution
    region: Option<RegionRequest>,
    /// Downscale so the image is at most this wide
    max_width: Option<u32>,
    /// True only when the user asked for this screenshot without saying what to capture: they
    /// then choose the whole screen, a monitor or a window. Leave false when you capture for your
    /// own purposes, so you are never kept waiting.
    ask_user_what_to_capture: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecordingRequest {
    /// Record this whole output, such as "DP-1"
    output: Option<String>,
    /// Record the screen area of this window; it must be on screen
    window_id: Option<String>,
    /// True only when the user asked for this recording without saying what to record: they then
    /// choose a monitor or a window. Leave false when you start a recording yourself, so you are
    /// never kept waiting.
    ask_user_what_to_capture: Option<bool>,
    /// Also record audio (default false)
    audio: Option<bool>,
    /// Stop automatically after this many seconds, 1 to 3600 (default 30). Pass the length the
    /// user asked for, if any.
    duration_seconds: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TextSearchRequest {
    /// Text to look for on screen, matched case-insensitively
    target: String,
    /// Restrict the search to this output
    output: Option<String>,
    /// Restrict the search to this window; faster and more precise than searching a monitor
    window_id: Option<String>,
    /// Restrict the search to this rectangle of the screen
    region: Option<RegionRequest>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TextWaitRequest {
    /// Text to wait for, matched case-insensitively
    target: String,
    /// Watch only this output
    output: Option<String>,
    /// Watch only this window
    window_id: Option<String>,
    /// Watch only this rectangle of the screen
    region: Option<RegionRequest>,
    /// Give up after this many milliseconds (default 10000, at most 30000)
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChangeWaitRequest {
    /// Watch only this output
    output: Option<String>,
    /// Watch only this window
    window_id: Option<String>,
    /// Watch only this rectangle of the screen
    region: Option<RegionRequest>,
    /// Give up after this many milliseconds (default 10000, at most 30000)
    timeout_ms: Option<u64>,
    /// How long the picture must stay still to count as settled, in milliseconds (default 500,
    /// at most 5000)
    settle_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ActionFilter {
    /// Only list actions whose name, description or group contains this text, such as "workspace"
    filter: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OptionQuery {
    /// Words that must all appear in the option path, such as "gap" or "blur enabled"; omit for a
    /// per-section summary
    query: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ConfigSection {
    /// Only return this section and its subsections, such as "keybinds", "layout" or "window_rule"
    section: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeybindRequest {
    /// Key chord as Umbriel writes it, such as "Mod+F" or "Mod+Shift+Return"
    chord: String,
    /// Umbriel action, as listed by list_actions, such as "window-toggle-fullscreen" or "spawn:kitty"
    action: String,
    /// Repeat the action while the keys are held; omit to keep Umbriel's default
    repeat: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OptionChange {
    /// Setting key from config_options, such as "layout.gap" or "output.DP-1.scale"
    key: String,
    /// New value: a number, true/false, a string, or a list, matching the setting's type
    value: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OptionKey {
    /// Setting key to reset to Umbriel's default, such as "layout.gap"
    key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct NewRule {
    /// Rule list: window_rule, layer_rule, workspace, scratchpad, input.device or
    /// security_context_rule
    list: String,
    /// The rule as a JSON object, such as {"match": {"app_id": "^mpv$"}, "default_floating": true};
    /// config_options with the list name as query shows the fields
    rule: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReplacedRule {
    /// Rule list, such as window_rule
    list: String,
    /// Which entry, counting from 1 in the order read_config shows them
    position: usize,
    /// The complete new rule as a JSON object; it replaces the whole entry
    rule: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RulePosition {
    /// Rule list, such as window_rule
    list: String,
    /// Which entry, counting from 1 in the order read_config shows them
    position: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ConfirmationCode {
    /// Confirmation code returned with the previewed change
    confirmation: String,
    /// Whether the user wants a backup of the previous config kept; omit to keep one
    backup: Option<bool>,
    /// True only if the user explicitly said to stop asking for config changes for the rest of
    /// this session; never set it on your own
    allow_rest_of_session: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeybindChord {
    /// Key chord to unbind, such as "Mod+F"
    chord: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClipboardWriteRequest {
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LayoutRequest {
    /// scrolling, dwindle, master or stacking (floating on older Umbriel)
    mode: String,
}

#[derive(Clone)]
struct UmbrielServer {
    profile: Profile,
    keep_backups: usize,
    pending_changes: Arc<approval::PendingChanges>,
    recorder: Arc<recording::Recorder>,
    watcher: Arc<watching::Watcher>,
}

enum Chosen {
    Target {
        output: Option<String>,
        window_id: Option<String>,
        seconds: Option<u32>,
    },
    Nothing(String),
}

fn focused_or_first_output() -> anyhow::Result<String> {
    let outputs = ipc::listed("outputs");
    let enabled = || {
        outputs.iter().filter(|output| {
            output.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
        })
    };
    enabled()
        .find(|output| output.get("focused").and_then(serde_json::Value::as_bool) == Some(true))
        .or_else(|| enabled().next())
        .and_then(|output| output.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Umbriel reports no enabled output to record"))
}

fn recording_area(
    output: Option<String>,
    window_id: Option<String>,
) -> anyhow::Result<(capture_target::RecordingArea, String)> {
    if let Some(id) = window_id {
        let placement = ipc::window_placement(&id)?;
        if !placement.shown_on_screen {
            return Err(anyhow::anyhow!(
                "window {id} is not on screen; a recording captures a screen area, so switch to \
                 its workspace first or record a monitor"
            ));
        }
        let description = format!("the screen area of window {id} ({})", placement.geometry);
        return Ok((
            capture_target::RecordingArea::Region {
                geometry: placement.geometry,
            },
            description,
        ));
    }
    let name = match output {
        Some(name) => name,
        None => focused_or_first_output()?,
    };
    let description = format!("monitor {name}");
    Ok((capture_target::RecordingArea::Output(name), description))
}

impl UmbrielServer {
    fn new(profile: Profile, keep_backups: u32) -> Self {
        Self {
            profile,
            keep_backups: keep_backups as usize,
            pending_changes: Arc::default(),
            recorder: Arc::default(),
            watcher: Arc::default(),
        }
    }

    async fn choose_target(
        &self,
        peer: &Peer<RoleServer>,
        output: &Option<String>,
        window_id: &Option<String>,
        ask_user: Option<bool>,
        purpose: Purpose,
        seconds: Option<u32>,
    ) -> Result<Chosen, ErrorData> {
        let already_chosen = output.is_some() || window_id.is_some();
        if already_chosen || ask_user != Some(true) {
            return Ok(Chosen::Target {
                output: output.clone(),
                window_id: window_id.clone(),
                seconds,
            });
        }
        let ask_length = purpose == Purpose::Recording && seconds.is_none();
        let choices = capture_target::capture_choices(
            &ipc::listed("outputs"),
            &ipc::listed("windows"),
            purpose,
        );
        match approval::ask_what_to_capture(peer, purpose, &choices, ask_length)
            .await
            .map_err(failed)?
        {
            approval::CaptureAnswer::Chosen {
                target,
                seconds: chosen_seconds,
            } => {
                let seconds = seconds.or(chosen_seconds);
                let (output, window_id) = match target {
                    CaptureTarget::WholeScreen => (None, None),
                    CaptureTarget::Output(name) => (Some(name), None),
                    CaptureTarget::Window(id) => (None, Some(id)),
                };
                Ok(Chosen::Target {
                    output,
                    window_id,
                    seconds,
                })
            }
            approval::CaptureAnswer::NotChosen(message) => Ok(Chosen::Nothing(message)),
            approval::CaptureAnswer::NoDialog => {
                let length_question = if ask_length {
                    format!(
                        " Also ask how long to record (default {}) and pass it as \
                         duration_seconds.",
                        capture_target::human_duration(capture_target::DEFAULT_RECORDING_SECONDS)
                    )
                } else {
                    String::new()
                };
                Ok(Chosen::Nothing(format!(
                    "Nothing was captured yet. This client cannot ask the user directly, so ask \
                     them what to capture, then call this tool again with their choice and \
                     ask_user_what_to_capture unset.{length_question}\n{}",
                    capture_target::choices_for_the_model(&choices)
                )))
            }
        }
    }

    fn permit_setting(
        &self,
        tool: &str,
        key: &str,
        value: Option<&serde_json::Value>,
    ) -> Result<(), ErrorData> {
        match setting::policy(key) {
            setting::Policy::UseKeybindTools => Err(ErrorData::invalid_params(
                format!(
                    "`{}` is a keybind; use set_keybind or remove_keybind for keybinds",
                    key.trim()
                ),
                None,
            )),
            setting::Policy::RunsProgramsOrLoadsFiles | setting::Policy::WeakensSecurity
                if value.is_some() && self.profile < Profile::Full =>
            {
                Err(refused(format!(
                    "`{tool}` cannot change `{}` in the `{}` profile: it can start programs, load \
                     files, or weaken a security check. Restart umbriel-mcp with `--profile full` to \
                     allow it.",
                    key.trim(),
                    self.profile
                )))
            }
            setting::Policy::TakesAnAction => match value.and_then(serde_json::Value::as_str) {
                Some(action) => self.profile.permit_action(tool, action).map_err(refused),
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    fn permit_rule(
        &self,
        tool: &str,
        list: &str,
        fields: &[(String, toml_edit::Value)],
    ) -> Result<(), ErrorData> {
        match rules::needs_full_profile(list.trim(), fields) {
            Some(reason) if self.profile < Profile::Full => Err(refused(format!(
                "`{tool}` cannot change this {} rule in the `{}` profile: {reason}. Restart \
                 umbriel-mcp with `--profile full` to allow it.",
                list.trim(),
                self.profile
            ))),
            _ => Ok(()),
        }
    }

    async fn area_to_capture(
        &self,
        peer: &Peer<RoleServer>,
        request: &CaptureRequest,
    ) -> Result<Result<screen::Area, String>, ErrorData> {
        let area = area_from(
            request.output.clone(),
            request.window_id.clone(),
            request.region.clone(),
        )?;
        if area != screen::Area::WholeScreen {
            return Ok(Ok(area));
        }
        match self
            .choose_target(
                peer,
                &None,
                &None,
                request.ask_user_what_to_capture,
                Purpose::Screenshot,
                None,
            )
            .await?
        {
            Chosen::Target {
                output, window_id, ..
            } => Ok(Ok(area_from(output, window_id, None)?)),
            Chosen::Nothing(message) => Ok(Err(message)),
        }
    }

    async fn approve_and_write(
        &self,
        peer: &Peer<RoleServer>,
        change: config_edit::PreparedChange,
    ) -> Result<String, ErrorData> {
        approval::ask_then_commit(peer, &self.pending_changes, change, self.keep_backups)
            .await
            .map_err(failed)
    }
}

#[tool_router]
impl UmbrielServer {
    #[tool(
        description = "List the windows open on this desktop with id, app_id, title, geometry, monitor and workspace; filter by workspace, output, app_id or pid. Use it to answer what is open or what is on workspace 4; pass workspace \"4\" for that workspace on every monitor. has_keyboard_focus is true for the one window that receives typing. on_screen is fully, partly, scrolled_out_of_view (on the shown workspace but scrolled away) or hidden (another workspace, minimized or a hidden tab). Umbriel's own visible means only that its workspace is shown, and its focused is each workspace's remembered focus, true on several windows at once; detail adds those and every other field Umbriel reports."
    )]
    fn list_windows(
        &self,
        Parameters(filter): Parameters<WindowFilter>,
    ) -> Result<String, ErrorData> {
        let all = ipc::windows().map_err(failed)?;
        let kept: Vec<_> =
            all.into_iter()
                .filter(|entry| {
                    let field = |key: &str| entry.get(key).and_then(|v| v.as_str()).unwrap_or("");
                    filter
                        .workspace
                        .as_ref()
                        .is_none_or(|wanted| workspace_matches(field("workspace"), wanted))
                        && filter
                            .output
                            .as_ref()
                            .is_none_or(|wanted| field("output") == wanted)
                        && filter.app_id.as_ref().is_none_or(|wanted| {
                            field("app_id").eq_ignore_ascii_case(wanted.trim())
                        })
                        && filter.pid.is_none_or(|wanted| {
                            entry.get("pid").and_then(serde_json::Value::as_i64) == Some(wanted)
                        })
                })
                .collect();
        let keyboard_focus = focused_window_id();
        let outputs = ipc::listed("outputs");
        as_pretty_json(listing::shown(
            serde_json::Value::Array(kept),
            filter.detail.unwrap_or(false),
            |window| listing::brief_window(window, keyboard_focus.as_deref(), &outputs),
        ))
    }

    #[tool(
        description = "List this desktop's workspaces with monitor, index, layout, window count and focus. Each monitor has its own numbered workspaces."
    )]
    fn list_workspaces(&self) -> Result<String, ErrorData> {
        as_pretty_json(ipc::query("workspaces").map_err(failed)?)
    }

    #[tool(
        description = "List this desktop's monitors (outputs): name, position, size, current mode, scale, focus and the workspace each one shows. Pass detail for every mode and field."
    )]
    fn list_outputs(
        &self,
        Parameters(request): Parameters<DetailRequest>,
    ) -> Result<String, ErrorData> {
        as_pretty_json(listing::shown(
            ipc::query("outputs").map_err(failed)?,
            request.detail.unwrap_or(false),
            listing::brief_output,
        ))
    }

    #[tool(description = "Get the window that currently has focus on this desktop.")]
    fn get_active_window(
        &self,
        Parameters(request): Parameters<DetailRequest>,
    ) -> Result<String, ErrorData> {
        let focused = ipc::focused_window().map_err(failed)?;
        let outputs = ipc::listed("outputs");
        let focused_id = focused
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        as_pretty_json(listing::shown(
            focused,
            request.detail.unwrap_or(false),
            |window| listing::brief_window(window, focused_id.as_deref(), &outputs),
        ))
    }

    #[tool(description = "List layer-shell surfaces such as bars, panels and wallpapers.")]
    fn list_layers(&self) -> Result<String, ErrorData> {
        as_pretty_json(ipc::query("layers").map_err(failed)?)
    }

    #[tool(
        description = "Switch to a workspace by position or name, optionally scoped to an output."
    )]
    fn switch_workspace(
        &self,
        Parameters(target): Parameters<WorkspaceTarget>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::on_workspace(
            "workspace-switch",
            &target.workspace,
            target.output.as_deref(),
        ))
    }

    #[tool(
        description = "Focus a window by id, optionally warping the pointer onto it. Returns only once the window has keyboard focus, so typing right after goes to it; fails if focus does not move (for example while the screen is locked)."
    )]
    async fn focus_window(
        &self,
        Parameters(target): Parameters<FocusTarget>,
    ) -> Result<String, ErrorData> {
        let verb = if target.warp_cursor.unwrap_or(false) {
            "window-focus-warp"
        } else {
            "window-focus"
        };
        let window_id = target.window_id.trim().to_owned();
        ipc::perform(&actions::CompositorAction::on_window(
            verb,
            Some(&window_id),
        ))
        .map_err(failed)?;
        let waited = tokio::task::spawn_blocking(move || wait_for_keyboard_focus(&window_id))
            .await
            .map_err(|error| failed(anyhow::anyhow!("the focus check failed: {error}")))?;
        waited.map_err(failed)
    }

    #[tool(description = "Ask a window to close; acts on the focused window when no id is given.")]
    fn close_window(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::on_window(
            "window-close",
            target.window_id.as_deref(),
        ))
    }

    #[tool(
        description = "Kill the process owning a window; acts on the focused window when no id is given."
    )]
    fn kill_window(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::on_window(
            "window-kill",
            target.window_id.as_deref(),
        ))
    }

    #[tool(description = "Move the focused window to a workspace, optionally scoped to an output.")]
    fn move_window_to_workspace(
        &self,
        Parameters(target): Parameters<WorkspaceTarget>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::on_workspace(
            "window-move-to-workspace",
            &target.workspace,
            target.output.as_deref(),
        ))
    }

    #[tool(description = "Toggle fullscreen for the focused window.")]
    fn toggle_fullscreen(&self) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::plain("window-toggle-fullscreen"))
    }

    #[tool(description = "Float or tile a window; acts on the focused window when no id is given.")]
    fn toggle_floating(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::on_window(
            "window-toggle-floating",
            target.window_id.as_deref(),
        ))
    }

    #[tool(
        description = "Resize the focused window by moving one edge by a signed fraction (-0.9 to 0.9)."
    )]
    fn resize_window(
        &self,
        Parameters(request): Parameters<ResizeRequest>,
    ) -> Result<String, ErrorData> {
        let verb = match request.edge.as_str() {
            "left" => "window-modify-width-left",
            "right" => "window-modify-width-right",
            "up" => "window-modify-height-up",
            "down" => "window-modify-height-down",
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown edge {other}; expected left, right, up or down"),
                    None,
                ));
            }
        };
        acted(actions::CompositorAction::by_delta(verb, request.delta))
    }

    #[tool(
        description = "Set the active workspace layout: scrolling, dwindle, master or stacking (older Umbriel calls stacking floating; either name works)."
    )]
    fn set_workspace_layout(
        &self,
        Parameters(request): Parameters<LayoutRequest>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::set_layout(&request.mode))
    }

    #[tool(description = "Toggle the workspace overview.")]
    fn toggle_overview(&self) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::plain("overview-toggle"))
    }

    #[tool(description = "Launch a command through the compositor with an activation token.")]
    fn launch_app(
        &self,
        Parameters(request): Parameters<SpawnRequest>,
    ) -> Result<String, ErrorData> {
        acted(actions::CompositorAction::spawn(&request.command))
    }

    #[tool(
        description = "Run any Umbriel action, for anything the other tools do not cover. Use list_actions for the names and argument syntax."
    )]
    fn run_action(&self, Parameters(request): Parameters<RawAction>) -> Result<String, ErrorData> {
        self.profile
            .permit_action("run_action", &request.action)
            .map_err(refused)?;
        ipc::perform_text(&request.action).map_err(failed)?;
        Ok(format!("ok: {}", request.action.trim()))
    }

    #[tool(
        description = "Move the pointer to absolute screen coordinates, or to x, y inside a window when window_id is given."
    )]
    fn mouse_move(
        &self,
        Parameters(position): Parameters<PointerPosition>,
    ) -> Result<String, ErrorData> {
        let (x, y) = match &position.window_id {
            Some(id) => point_in_window(id, position.x, position.y)?,
            None => (position.x, position.y),
        };
        move_pointer_to(x, y)?;
        Ok(format!("pointer at {x},{y}"))
    }

    #[tool(
        description = "Click a mouse button where the pointer is, at absolute x, y, or at x, y inside a window (window_id, coordinates from its top-left; refused if it is not on screen). Reports which window has focus afterwards."
    )]
    fn mouse_click(
        &self,
        Parameters(request): Parameters<ClickRequest>,
    ) -> Result<String, ErrorData> {
        let button = request.button.unwrap_or_else(|| "left".to_string());
        let target = pointer_target(request.window_id.as_deref(), request.x, request.y)?;
        if let Some((x, y)) = target {
            move_pointer_to(x, y)?;
        }
        input::click(&button, request.double.unwrap_or(false)).map_err(failed)?;
        let place = target
            .map(|(x, y)| format!(" at screen ({x}, {y})"))
            .unwrap_or_default();
        Ok(format!("clicked {button}{place}. {}", focus_report()))
    }

    #[tool(description = "Scroll the mouse wheel at the current pointer position.")]
    fn mouse_scroll(
        &self,
        Parameters(request): Parameters<ScrollRequest>,
    ) -> Result<String, ErrorData> {
        let direction = request.direction.unwrap_or_else(|| "down".to_string());
        let amount = request.amount.unwrap_or(3);
        input::scroll(&direction, amount).map_err(failed)?;
        Ok(format!("scrolled {direction} by {amount}"))
    }

    #[tool(description = "Press at one point, drag to another, and release.")]
    fn mouse_drag(
        &self,
        Parameters(request): Parameters<DragRequest>,
    ) -> Result<String, ErrorData> {
        let button = request.button.unwrap_or_else(|| "left".to_string());
        input::drag(
            (request.from_x, request.from_y),
            (request.to_x, request.to_y),
            &button,
        )
        .map_err(failed)?;
        Ok(format!(
            "dragged {button} from {},{} to {},{}",
            request.from_x, request.from_y, request.to_x, request.to_y
        ))
    }

    #[tool(
        description = "Type literal text into the focused surface. Pass window_id to type only if that window has keyboard focus, so text never lands in another window. Reports where it went."
    )]
    fn type_text(&self, Parameters(request): Parameters<TypeRequest>) -> Result<String, ErrorData> {
        if let Some(id) = &request.window_id {
            require_keyboard_focus(id, "nothing was typed")?;
        }
        input::type_text(&request.text, request.delay_ms.unwrap_or(0)).map_err(failed)?;
        Ok(format!(
            "typed {} characters. {}",
            request.text.chars().count(),
            focus_report()
        ))
    }

    #[tool(
        description = "Press a key chord such as ctrl+c, alt+Tab or Return. Pass window_id to press only if that window has keyboard focus. Reports which window has focus afterwards."
    )]
    fn key_press(&self, Parameters(request): Parameters<KeyRequest>) -> Result<String, ErrorData> {
        if let Some(id) = &request.window_id {
            require_keyboard_focus(id, "nothing was pressed")?;
        }
        input::press_keys(&request.keys).map_err(failed)?;
        Ok(format!("pressed {}. {}", request.keys, focus_report()))
    }

    #[tool(
        description = "Capture the screen, one output, one window or one region as a downscaled image, with its capture time. Prefer window_id: it captures that window even on a hidden workspace, without switching to it, and leaves out the user's other windows. Use region to zoom in on a detail at full resolution. When the user asks for a screenshot without saying what to capture, set ask_user_what_to_capture so they choose; when you capture for yourself, leave it unset."
    )]
    async fn screenshot(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<CaptureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let area = match self.area_to_capture(&peer, &request).await? {
            Ok(area) => area,
            Err(message) => return Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
        };
        let max_width = request.max_width.unwrap_or(screen::DEFAULT_MAX_WIDTH);
        let (shot, notes) = self.watcher.capture(&area, max_width).map_err(failed)?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(
                BASE64_STANDARD.encode(shot.jpeg().map_err(failed)?),
                "image/jpeg",
            ),
            ContentBlock::text(capture_description(&shot, &notes)),
        ]))
    }

    #[tool(
        description = "Capture the screen, one output, one window or one region and return the text found in it by OCR. A window_id reads that window even on a hidden workspace, without switching to it. When the user asks for this without saying what to capture, set ask_user_what_to_capture so they choose; when you capture for yourself, leave it unset."
    )]
    async fn screenshot_with_ocr(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<CaptureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let area = match self.area_to_capture(&peer, &request).await? {
            Ok(area) => area,
            Err(message) => return Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
        };
        let max_width = request.max_width.unwrap_or(screen::DEFAULT_MAX_WIDTH);
        let (shot, notes) = self.watcher.capture(&area, max_width).map_err(failed)?;
        let recognised = screen::read_text(&shot.native_png).map_err(failed)?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(
                BASE64_STANDARD.encode(shot.jpeg().map_err(failed)?),
                "image/jpeg",
            ),
            ContentBlock::text(format!(
                "{}\n\nOCR text:\n{recognised}",
                capture_description(&shot, &notes)
            )),
        ]))
    }

    #[tool(
        description = "Find text on screen and report absolute screen coordinates for each match. Pass window_id or region to search only there, which is faster and avoids matches elsewhere; \"no match\" comes back quickly instead of waiting. Icon-only buttons such as a close cross have no text to find."
    )]
    fn find_text_on_screen(
        &self,
        Parameters(request): Parameters<TextSearchRequest>,
    ) -> Result<String, ErrorData> {
        let area = area_from(request.output, request.window_id, request.region)?;
        let (shot, _) = self
            .watcher
            .capture(&area, screen::DEFAULT_MAX_WIDTH)
            .map_err(failed)?;
        let found = screen::find_text(&shot.native_png, &request.target).map_err(failed)?;
        if found.is_empty() {
            return Ok(format!(
                "no match for {:?} in {}",
                request.target,
                area.describe()
            ));
        }
        Ok(matches_described(&shot, &found))
    }

    #[tool(
        description = "Find text on screen and left-click its first match, then report which window has focus. Pass window_id or region to search only there; the window must be on screen."
    )]
    fn click_text(
        &self,
        Parameters(request): Parameters<TextSearchRequest>,
    ) -> Result<String, ErrorData> {
        let area = area_from(request.output, request.window_id, request.region)?;
        let (shot, _) = self
            .watcher
            .capture(&area, screen::DEFAULT_MAX_WIDTH)
            .map_err(failed)?;
        if !shot.shown_on_screen {
            return Err(ErrorData::invalid_params(
                format!(
                    "{} is not on screen, so a click would land on whatever covers it; switch to \
                     its workspace or call focus_window first",
                    area.describe()
                ),
                None,
            ));
        }
        let found = screen::find_text(&shot.native_png, &request.target).map_err(failed)?;
        let best = found.first().ok_or_else(|| {
            ErrorData::internal_error(
                format!("no match for {:?} in {}", request.target, area.describe()),
                None,
            )
        })?;
        let (x, y) = shot.screen_point(best.center_x, best.center_y);
        if let screen::Area::Window(id) = &area {
            let placement = ipc::window_placement(id).map_err(failed)?;
            refuse_unreachable_point(id, &placement, x, y)?;
        }
        move_pointer_to(x as i32, y as i32)?;
        input::click("left", false).map_err(failed)?;
        Ok(format!(
            "clicked {:?} at ({x}, {y}). {}",
            best.text,
            focus_report()
        ))
    }

    #[tool(
        description = "Wait until text appears on screen, for example after a click or a page load, instead of sleeping and re-capturing. Returns the matches with screen coordinates, or \"no match\" at the timeout (default 10 s, at most 30 s). Pass window_id or region to watch only there."
    )]
    async fn wait_for_text(
        &self,
        Parameters(request): Parameters<TextWaitRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let area = area_from(request.output, request.window_id, request.region)?;
        let timeout = watching::bounded(
            request.timeout_ms,
            watching::DEFAULT_WAIT,
            watching::LONGEST_WAIT,
        );
        let watcher = Arc::clone(&self.watcher);
        let target = request.target.clone();
        let waited_area = area.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            watcher.wait_for_text(&waited_area, &target, timeout, screen::DEFAULT_MAX_WIDTH)
        })
        .await
        .map_err(|cause| failed(cause.into()))?
        .map_err(failed)?;
        let text = match outcome {
            watching::TextWait::Found {
                matches,
                shot,
                notes,
                after,
            } => {
                let mut lines = vec![
                    format!(
                        "{:?} appeared after {}.",
                        request.target,
                        watching::seconds(after)
                    ),
                    matches_described(&shot, &matches),
                ];
                lines.extend(notes);
                lines.join("\n")
            }
            watching::TextWait::NotFound { attempts } => format!(
                "no match for {:?} in {} within {} ({attempts} looks)",
                request.target,
                area.describe(),
                watching::seconds(timeout)
            ),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Wait until the screen changes and then settles, for example after a click, a key press or a reload, and return the settled picture. It compares with your previous capture of the same window, monitor or region, so capture it before acting; that tells \"nothing happened\" apart from \"not updated yet\". Timeout default 10 s, at most 30 s."
    )]
    async fn wait_for_change(
        &self,
        Parameters(request): Parameters<ChangeWaitRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let area = area_from(request.output, request.window_id, request.region)?;
        let timeout = watching::bounded(
            request.timeout_ms,
            watching::DEFAULT_WAIT,
            watching::LONGEST_WAIT,
        );
        let settle = watching::bounded(
            request.settle_ms,
            watching::DEFAULT_SETTLE,
            watching::LONGEST_SETTLE,
        );
        let watcher = Arc::clone(&self.watcher);
        let waited_area = area.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            watcher.wait_for_change(&waited_area, timeout, settle, screen::DEFAULT_MAX_WIDTH)
        })
        .await
        .map_err(|cause| failed(cause.into()))?
        .map_err(failed)?;
        let (shot, notes, summary) = match outcome {
            watching::ChangeWait::Settled {
                shot,
                notes,
                changed_after,
                settled_after,
            } => {
                let summary = format!(
                    "{} changed after {} and settled after {}.",
                    area.describe(),
                    watching::seconds(changed_after),
                    watching::seconds(settled_after)
                );
                (shot, notes, summary)
            }
            watching::ChangeWait::StillChanging {
                shot,
                notes,
                changed_after,
            } => {
                let summary = format!(
                    "{} changed after {} but was still changing at the {} timeout (an \
                     animation or a loading indicator); this is the latest picture.",
                    area.describe(),
                    watching::seconds(changed_after),
                    watching::seconds(timeout)
                );
                (shot, notes, summary)
            }
            watching::ChangeWait::Unchanged {
                since,
                compared_with_earlier_capture,
            } => {
                let compared_with = if compared_with_earlier_capture {
                    "your previous capture"
                } else {
                    "the first look (there was no earlier capture of it to compare with)"
                };
                return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "{} did not change within {}; it still matches {compared_with} from {}.",
                    area.describe(),
                    watching::seconds(timeout),
                    screen::clock_time(since)
                ))]));
            }
        };
        Ok(CallToolResult::success(vec![
            ContentBlock::image(
                BASE64_STANDARD.encode(shot.jpeg().map_err(failed)?),
                "image/jpeg",
            ),
            ContentBlock::text(format!("{summary}\n{}", capture_description(&shot, &notes))),
        ]))
    }

    #[tool(
        description = "Describe the Umbriel compositor itself: installed version, keyboard layouts, active keybind submap, color management and tearing state."
    )]
    fn umbriel_info(&self) -> Result<String, ErrorData> {
        as_pretty_json(compositor::info())
    }

    #[tool(
        description = "List the actions Umbriel can perform (what run_action and keybinds accept), with descriptions. Filter by text such as \"workspace\" or \"focus\"."
    )]
    fn list_actions(
        &self,
        Parameters(request): Parameters<ActionFilter>,
    ) -> Result<String, ErrorData> {
        compositor::list_actions(request.filter.as_deref()).map_err(failed)
    }

    #[tool(
        description = "Search Umbriel's configuration options by path, such as \"gap\" or \"blur\", returning exact key names, types, defaults, ranges and allowed values. Use it before suggesting config changes."
    )]
    fn config_options(
        &self,
        Parameters(request): Parameters<OptionQuery>,
    ) -> Result<String, ErrorData> {
        as_pretty_json(compositor::config_options(request.query.as_deref()).map_err(failed)?)
    }

    #[tool(
        description = "Read the user's Umbriel config, or one section such as \"keybinds\", together with whether it validates. Use it for questions like what a key does or how a setting is configured."
    )]
    fn read_config(
        &self,
        Parameters(request): Parameters<ConfigSection>,
    ) -> Result<String, ErrorData> {
        compositor::read_config(request.section.as_deref()).map_err(failed)
    }

    #[tool(
        description = "Bind a key chord to an Umbriel action in the user's config, replacing any existing binding for it. The change is checked with `umbriel config validate` and is only written after the user approves it and they choose whether to keep a backup. Use list_actions for valid actions.",
        annotations(destructive_hint = true)
    )]
    async fn set_keybind(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<KeybindRequest>,
    ) -> Result<String, ErrorData> {
        self.profile
            .permit_action("set_keybind", &request.action)
            .map_err(refused)?;
        match config_edit::prepare_set_keybind(&request.chord, &request.action, request.repeat)
            .map_err(failed)?
        {
            Some(change) => {
                let starts_a_program = profile::is_spawn_action(&request.action)
                    .then_some("it binds a key to start a program");
                self.approve_and_write(&peer, change.asking_every_time_because(starts_a_program))
                    .await
            }
            None => Ok(format!(
                "{} is already bound to {}; nothing changed",
                request.chord.trim(),
                request.action.trim()
            )),
        }
    }

    #[tool(
        description = "Remove a key chord's binding from the user's Umbriel config. Only written after the user approves it, and they choose whether to keep a backup.",
        annotations(destructive_hint = true)
    )]
    async fn remove_keybind(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<KeybindChord>,
    ) -> Result<String, ErrorData> {
        let change = config_edit::prepare_remove_keybind(&request.chord).map_err(failed)?;
        self.approve_and_write(&peer, change).await
    }

    #[tool(
        description = "Change one Umbriel setting in the user's config, such as \"layout.gap\" to 12 or \"appearance.blur.enabled\" to false. Keys and values are checked against Umbriel's schema (see config_options) and with `umbriel config validate`; the change is only written after the user approves it, and they choose whether to keep a backup. Not for keybinds or rule lists.",
        annotations(destructive_hint = true)
    )]
    async fn set_option(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<OptionChange>,
    ) -> Result<String, ErrorData> {
        self.permit_setting("set_option", &request.key, Some(&request.value))?;
        match setting::prepare_set_option(&request.key, &request.value).map_err(failed)? {
            Some(change) => {
                let reason = setting_that_always_asks(&request.key, &request.value);
                self.approve_and_write(&peer, change.asking_every_time_because(reason))
                    .await
            }
            None => Ok(format!(
                "{} already has that value; nothing changed",
                request.key.trim()
            )),
        }
    }

    #[tool(
        description = "Remove one setting from the user's Umbriel config so Umbriel uses its default again. Only written after the user approves it, and they choose whether to keep a backup.",
        annotations(destructive_hint = true)
    )]
    async fn reset_option(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<OptionKey>,
    ) -> Result<String, ErrorData> {
        self.permit_setting("reset_option", &request.key, None)?;
        let change = setting::prepare_reset_option(&request.key).map_err(failed)?;
        self.approve_and_write(&peer, change).await
    }

    #[tool(
        description = "Add a rule to one of Umbriel's rule lists (window_rule, layer_rule, workspace, scratchpad, input.device, security_context_rule) in the user's config. Fields are checked against Umbriel's schema and the config with `umbriel config validate`; the change is only written after the user approves it, and they choose whether to keep a backup.",
        annotations(destructive_hint = true)
    )]
    async fn add_rule(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<NewRule>,
    ) -> Result<String, ErrorData> {
        let fields = rules::fields_for(&request.list, &request.rule).map_err(failed)?;
        self.permit_rule("add_rule", &request.list, &fields)?;
        let reason = rules::needs_full_profile(request.list.trim(), &fields);
        match rules::prepare_add(request.list.trim(), fields).map_err(failed)? {
            Some(change) => {
                self.approve_and_write(&peer, change.asking_every_time_because(reason))
                    .await
            }
            None => Ok("that rule would not change the config; nothing changed".to_owned()),
        }
    }

    #[tool(
        description = "Replace one rule in a rule list of the user's Umbriel config with a complete new rule. Use read_config first to see the entries; position counts from 1. Only written after the user approves it, and they choose whether to keep a backup.",
        annotations(destructive_hint = true)
    )]
    async fn replace_rule(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<ReplacedRule>,
    ) -> Result<String, ErrorData> {
        let fields = rules::fields_for(&request.list, &request.rule).map_err(failed)?;
        self.permit_rule("replace_rule", &request.list, &fields)?;
        let reason = rules::needs_full_profile(request.list.trim(), &fields);
        match rules::prepare_replace(request.list.trim(), request.position, fields)
            .map_err(failed)?
        {
            Some(change) => {
                self.approve_and_write(&peer, change.asking_every_time_because(reason))
                    .await
            }
            None => Ok("the new rule is identical to the old one; nothing changed".to_owned()),
        }
    }

    #[tool(
        description = "Remove one rule from a rule list of the user's Umbriel config. Use read_config first to see the entries; position counts from 1. Only written after the user approves it, and they choose whether to keep a backup.",
        annotations(destructive_hint = true)
    )]
    async fn remove_rule(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<RulePosition>,
    ) -> Result<String, ErrorData> {
        self.permit_rule("remove_rule", &request.list, &[])?;
        let change = rules::prepare_remove(&request.list, request.position).map_err(failed)?;
        self.approve_and_write(&peer, change).await
    }

    #[tool(
        description = "Write a config change that the user has explicitly approved, using the confirmation code a config tool returned. Only call this after showing the user the change and getting their yes; never approve on their behalf.",
        annotations(destructive_hint = true)
    )]
    fn apply_config_change(
        &self,
        Parameters(request): Parameters<ConfirmationCode>,
    ) -> Result<String, ErrorData> {
        let change = self
            .pending_changes
            .take(&request.confirmation)
            .map_err(failed)?;
        let keep_backup = request
            .backup
            .unwrap_or_else(approval::keep_backup_unless_told_otherwise);
        if request.allow_rest_of_session == Some(true) {
            self.pending_changes.allow_rest_of_session(keep_backup);
        }
        config_edit::commit(
            &change,
            config_edit::BackupPolicy {
                keep_backup,
                keep_newest: self.keep_backups,
            },
        )
        .map_err(failed)
    }

    #[tool(
        description = "Start recording a video of one monitor or one on-screen window to a file in the user's Videos folder. When the user asks for a recording without saying what to record, set ask_user_what_to_capture so they choose; when you start one yourself, give output or window_id, or leave both unset for the focused monitor. Stops by itself after duration_seconds (default 30); stop sooner with stop_recording."
    )]
    async fn start_recording(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<RecordingRequest>,
    ) -> Result<String, ErrorData> {
        let longest = capture_target::LONGEST_RECORDING_SECONDS;
        if let Some(seconds) = request.duration_seconds
            && !(1..=longest).contains(&seconds)
        {
            return Err(ErrorData::invalid_params(
                format!("duration_seconds must be between 1 and {longest}"),
                None,
            ));
        }
        let (output, window_id, seconds) = match self
            .choose_target(
                &peer,
                &request.output,
                &request.window_id,
                request.ask_user_what_to_capture,
                Purpose::Recording,
                request.duration_seconds,
            )
            .await?
        {
            Chosen::Target {
                output,
                window_id,
                seconds,
            } => (
                output,
                window_id,
                seconds.unwrap_or(capture_target::DEFAULT_RECORDING_SECONDS),
            ),
            Chosen::Nothing(message) => return Ok(message),
        };
        let (area, description) = recording_area(output, window_id).map_err(failed)?;
        let recorder = Arc::clone(&self.recorder);
        let audio = request.audio.unwrap_or(false);
        let started_description = description.clone();
        let started =
            tokio::task::spawn_blocking(move || recorder.start(area, started_description, audio))
                .await
                .map_err(|error| failed(anyhow::anyhow!("the recorder task failed: {error}")))?
                .map_err(failed)?;
        let recorder = Arc::clone(&self.recorder);
        let id = started.id;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(u64::from(seconds))).await;
            let _ = tokio::task::spawn_blocking(move || recorder.stop_at_time_limit(id)).await;
        });
        let audio_note = if audio {
            " with audio"
        } else {
            " without audio"
        };
        Ok(format!(
            "Recording {description}{audio_note} to {}. It stops by itself after {}; call \
             stop_recording to stop sooner.",
            started.file.display(),
            capture_target::human_duration(seconds)
        ))
    }

    #[tool(description = "Stop the current screen recording and report where the video was saved.")]
    async fn stop_recording(&self) -> Result<String, ErrorData> {
        let recorder = Arc::clone(&self.recorder);
        tokio::task::spawn_blocking(move || recorder.stop())
            .await
            .map_err(|error| failed(anyhow::anyhow!("the recorder task failed: {error}")))?
            .map_err(failed)
    }

    #[tool(description = "Read the current clipboard contents.")]
    fn clipboard_read(&self) -> Result<String, ErrorData> {
        input::clipboard_read().map_err(failed)
    }

    #[tool(description = "Replace the clipboard contents.")]
    fn clipboard_write(
        &self,
        Parameters(request): Parameters<ClipboardWriteRequest>,
    ) -> Result<String, ErrorData> {
        input::clipboard_write(&request.text).map_err(failed)?;
        Ok(format!(
            "copied {} characters",
            request.text.chars().count()
        ))
    }
}

#[tool_handler(
    name = "umbriel-mcp",
    instructions = "Inspect and control this desktop's windows, workspaces, monitors, input, screen and clipboard (Umbriel Wayland compositor).\n\nThis desktop runs Umbriel, not Hyprland, Sway, i3 or X11, so hyprctl, swaymsg, i3-msg, wmctrl and xdotool do not work here; answer questions about open windows, workspaces and monitors with these tools. Each monitor has its own numbered workspaces: list_windows with workspace \"4\" shows workspace 4 on every monitor. Window and workspace state come straight from Umbriel's IPC socket; use run_action for any compositor action without a dedicated tool. For questions about Umbriel itself, use umbriel_info (version and state), list_actions (what it can do), config_options (valid config keys) and read_config (the user's keybinds and settings) instead of guessing. Change settings with set_option and reset_option, keybinds with set_keybind and remove_keybind, and rule lists such as window_rule with add_rule, replace_rule and remove_rule, rather than editing the config file directly. Every config change needs the user's approval: where the client supports it the user is asked in a dialog; otherwise the tool returns a preview and a confirmation code, and you must show the user the change, ask whether to apply it and whether to keep a backup, and wait for their explicit answer before calling apply_config_change. Never approve a change on the user's behalf. When the user asks for a screenshot or a recording without saying what to capture, set ask_user_what_to_capture so they choose the whole screen, a monitor or a window; when you capture for your own purposes, leave it unset so you are never kept waiting.\n\nThe mouse and keyboard are the user's own, on a desktop they may be using at the same time. Ask the user before you click, type or press keys in their windows, unless they asked you to. Prefer capturing one window by window_id over whole monitors: it is sharper and leaves out their other windows. Pass window_id to type_text and key_press so nothing is typed when another window has focus, and to mouse_click to click at coordinates inside that window. After acting, use wait_for_change (it compares with your previous capture of the same window) or wait_for_text instead of sleeping and re-capturing. If focus or a window title changed without you, the user is probably using the desktop: stop and ask."
)]
impl ServerHandler for UmbrielServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.profile.permit_tool(&request.name).map_err(refused)?;
        let tool = request.name.to_string();
        let server = self.clone();
        tokio::spawn(async move {
            let tool_call = ToolCallContext::new(&server, request, context);
            Self::tool_router().call(tool_call).await
        })
        .await
        .unwrap_or_else(|crash| Err(crashed_tool_error(&tool, crash)))
    }
}

#[derive(Parser)]
#[command(version, about = "MCP server for the Umbriel Wayland compositor")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// How much authority the server grants: read-only observes only, no-exec adds input and
    /// window control but cannot start programs, full allows everything
    #[arg(long, value_enum, default_value_t = Profile::Full)]
    profile: Profile,

    /// How many config backups to keep; older umbriel-mcp backups are deleted when a new one is
    /// written
    #[arg(long, default_value_t = DEFAULT_KEPT_BACKUPS, value_parser = clap::value_parser!(u32).range(1..=1000))]
    keep_backups: u32,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Check that Umbriel, its input protocols, screen capture, and OCR are ready
    Doctor,
    /// Download and install the latest release in place of this binary
    Update {
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
    },
    /// Register this server with an AI coding harness, or print the config to paste
    Install {
        harness: register::Harness,
        /// Restrict the registered server to this profile (default: no restriction)
        #[arg(long, value_enum)]
        profile: Option<Profile>,
    },
    /// Unregister umbriel-mcp from every harness and remove this binary
    Uninstall {
        /// Do not ask before removing
        #[arg(long)]
        yes: bool,
        /// Only unregister; leave the umbriel-mcp binary installed
        #[arg(long)]
        keep_binary: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Doctor) => return Ok(doctor::run()),
        Some(Command::Update { check }) => return Ok(update::run(check)),
        Some(Command::Install { harness, profile }) => return Ok(register::run(harness, profile)),
        Some(Command::Uninstall { yes, keep_binary }) => {
            return Ok(uninstall::run(yes, keep_binary));
        }
        None => {}
    }
    let service = UmbrielServer::new(cli.profile, cli.keep_backups)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::UmbrielServer;
    use crate::profile::TOOL_ACCESS;
    use crate::register::SERVER_SUMMARY;
    use rmcp::ServerHandler;

    #[test]
    fn the_registration_summary_is_the_first_line_of_the_server_instructions() {
        let instructions =
            UmbrielServer::new(crate::profile::Profile::Full, super::DEFAULT_KEPT_BACKUPS)
                .get_info()
                .instructions
                .expect("server instructions");

        assert_eq!(instructions.lines().next(), Some(SERVER_SUMMARY));
    }

    #[test]
    fn every_advertised_tool_is_explicitly_classified_and_nothing_stale_remains() {
        let mut advertised: Vec<String> = UmbrielServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let mut classified: Vec<String> = TOOL_ACCESS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        advertised.sort();
        classified.sort();

        assert_eq!(
            advertised, classified,
            "TOOL_ACCESS must list exactly the advertised tools"
        );
    }
}
