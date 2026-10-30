mod doctor;
mod input;
mod ipc;
mod keyboard;
mod profile;
mod register;
mod screen;

use base64::prelude::{BASE64_STANDARD, Engine};
use clap::Parser;
use profile::Profile;
use rmcp::RoleServer;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

fn failed(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

fn as_pretty_json(value: serde_json::Value) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(&value).map_err(|cause| failed(cause.into()))
}

fn acted(action: String) -> Result<String, ErrorData> {
    ipc::dispatch(&action).map_err(failed)?;
    Ok(format!("ok: {action}"))
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

fn with_optional_target(base: &str, target: &Option<String>) -> String {
    match target {
        Some(value) if !value.is_empty() => format!("{base}:{value}"),
        _ => base.to_string(),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WindowFilter {
    /// Workspace to list: a number or name such as "4" (that workspace on every monitor),
    /// or a full id such as "HDMI-A-1:4"
    workspace: Option<String>,
    /// Only list windows on this output, such as "DP-1"
    output: Option<String>,
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
    delta: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RawAction {
    /// Any action accepted by `umbriel msg`, such as "overview-toggle"
    action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SpawnRequest {
    /// Command line to run
    command: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PointerPosition {
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClickRequest {
    /// left, right, middle or side
    button: Option<String>,
    /// Issue a double click
    double: Option<bool>,
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
    /// Delay between keystrokes in milliseconds
    delay_ms: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeyRequest {
    /// Chord such as "ctrl+c", "alt+Tab" or "Return"
    keys: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CaptureRequest {
    /// Capture only this output, such as "HDMI-A-1"
    output: Option<String>,
    /// Capture only this window id
    window_id: Option<String>,
    /// Downscale so the image is at most this wide
    max_width: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TextSearchRequest {
    /// Text to look for on screen, matched case-insensitively
    target: String,
    /// Restrict the search to this output
    output: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClipboardWriteRequest {
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LayoutRequest {
    /// scrolling, dwindle, master or floating
    mode: String,
}

#[derive(Clone)]
struct UmbrielServer {
    profile: Profile,
}

#[tool_router]
impl UmbrielServer {
    #[tool(
        description = "List the windows open on this desktop with id, app_id, title, geometry, monitor and workspace. Use it to answer what is open or what is on workspace 4; pass workspace \"4\" for that workspace on every monitor."
    )]
    fn list_windows(
        &self,
        Parameters(filter): Parameters<WindowFilter>,
    ) -> Result<String, ErrorData> {
        let all = ipc::windows().map_err(failed)?;
        let kept: Vec<_> = all
            .into_iter()
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
            })
            .collect();
        as_pretty_json(serde_json::Value::Array(kept))
    }

    #[tool(
        description = "List this desktop's workspaces with monitor, index, layout, window count and focus. Each monitor has its own numbered workspaces."
    )]
    fn list_workspaces(&self) -> Result<String, ErrorData> {
        as_pretty_json(ipc::query("workspaces").map_err(failed)?)
    }

    #[tool(
        description = "List this desktop's monitors (outputs) with position, modes, scale and the workspace each one shows."
    )]
    fn list_outputs(&self) -> Result<String, ErrorData> {
        as_pretty_json(ipc::query("outputs").map_err(failed)?)
    }

    #[tool(description = "Get the window that currently has focus on this desktop.")]
    fn get_active_window(&self) -> Result<String, ErrorData> {
        as_pretty_json(ipc::query("focused-window").map_err(failed)?)
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
        let selector = match &target.output {
            Some(output) => format!("{}/{}", target.workspace, output),
            None => target.workspace.clone(),
        };
        acted(format!("workspace-switch:{selector}"))
    }

    #[tool(description = "Focus a window by id, optionally warping the pointer onto it.")]
    fn focus_window(
        &self,
        Parameters(target): Parameters<FocusTarget>,
    ) -> Result<String, ErrorData> {
        let verb = if target.warp_cursor.unwrap_or(false) {
            "window-focus-warp"
        } else {
            "window-focus"
        };
        acted(format!("{verb}:{}", target.window_id))
    }

    #[tool(description = "Ask a window to close; acts on the focused window when no id is given.")]
    fn close_window(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(with_optional_target("window-close", &target.window_id))
    }

    #[tool(
        description = "Kill the process owning a window; acts on the focused window when no id is given."
    )]
    fn kill_window(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(with_optional_target("window-kill", &target.window_id))
    }

    #[tool(description = "Move the focused window to a workspace, optionally scoped to an output.")]
    fn move_window_to_workspace(
        &self,
        Parameters(target): Parameters<WorkspaceTarget>,
    ) -> Result<String, ErrorData> {
        let selector = match &target.output {
            Some(output) => format!("{}/{}", target.workspace, output),
            None => target.workspace.clone(),
        };
        acted(format!("window-move-to-workspace:{selector}"))
    }

    #[tool(description = "Toggle fullscreen for the focused window.")]
    fn toggle_fullscreen(&self) -> Result<String, ErrorData> {
        acted("window-toggle-fullscreen".to_string())
    }

    #[tool(description = "Float or tile a window; acts on the focused window when no id is given.")]
    fn toggle_floating(
        &self,
        Parameters(target): Parameters<WindowTarget>,
    ) -> Result<String, ErrorData> {
        acted(with_optional_target(
            "window-toggle-floating",
            &target.window_id,
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
        acted(format!("{verb}:{}", request.delta))
    }

    #[tool(
        description = "Set the active workspace layout: scrolling, dwindle, master or floating."
    )]
    fn set_workspace_layout(
        &self,
        Parameters(request): Parameters<LayoutRequest>,
    ) -> Result<String, ErrorData> {
        acted(format!("workspace-set-layout:{}", request.mode))
    }

    #[tool(description = "Toggle the workspace overview.")]
    fn toggle_overview(&self) -> Result<String, ErrorData> {
        acted("overview-toggle".to_string())
    }

    #[tool(description = "Launch a command through the compositor with an activation token.")]
    fn launch_app(
        &self,
        Parameters(request): Parameters<SpawnRequest>,
    ) -> Result<String, ErrorData> {
        acted(format!("spawn:{}", request.command))
    }

    #[tool(description = "Run any raw umbriel action, for anything the other tools do not cover.")]
    fn run_action(&self, Parameters(request): Parameters<RawAction>) -> Result<String, ErrorData> {
        self.profile
            .permit_action(&request.action)
            .map_err(refused)?;
        acted(request.action)
    }

    #[tool(description = "Move the pointer to absolute screen coordinates.")]
    fn mouse_move(
        &self,
        Parameters(position): Parameters<PointerPosition>,
    ) -> Result<String, ErrorData> {
        input::move_pointer(position.x, position.y).map_err(failed)?;
        Ok(format!("pointer at {},{}", position.x, position.y))
    }

    #[tool(description = "Click a mouse button at the current pointer position.")]
    fn mouse_click(
        &self,
        Parameters(request): Parameters<ClickRequest>,
    ) -> Result<String, ErrorData> {
        let button = request.button.unwrap_or_else(|| "left".to_string());
        input::click(&button, request.double.unwrap_or(false)).map_err(failed)?;
        Ok(format!("clicked {button}"))
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

    #[tool(description = "Type literal text into the focused surface.")]
    fn type_text(&self, Parameters(request): Parameters<TypeRequest>) -> Result<String, ErrorData> {
        input::type_text(&request.text, request.delay_ms.unwrap_or(0)).map_err(failed)?;
        Ok(format!("typed {} characters", request.text.chars().count()))
    }

    #[tool(description = "Press a key chord such as ctrl+c, alt+Tab or Return.")]
    fn key_press(&self, Parameters(request): Parameters<KeyRequest>) -> Result<String, ErrorData> {
        input::press_keys(&request.keys).map_err(failed)?;
        Ok(format!("pressed {}", request.keys))
    }

    #[tool(description = "Capture the screen, one output, or one window, as a downscaled image.")]
    fn screenshot(
        &self,
        Parameters(request): Parameters<CaptureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let shot = screen::capture(
            request.output.as_deref(),
            request.window_id.as_deref(),
            request.max_width.unwrap_or(screen::DEFAULT_MAX_WIDTH),
        )
        .map_err(failed)?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(BASE64_STANDARD.encode(&shot.jpeg), "image/jpeg"),
            ContentBlock::text(shot.coordinate_note()),
        ]))
    }

    #[tool(description = "Capture the screen and return the text found in it by OCR.")]
    fn screenshot_with_ocr(
        &self,
        Parameters(request): Parameters<CaptureRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let shot = screen::capture(
            request.output.as_deref(),
            request.window_id.as_deref(),
            request.max_width.unwrap_or(screen::DEFAULT_MAX_WIDTH),
        )
        .map_err(failed)?;
        let recognised = screen::read_text(&shot.native_png).map_err(failed)?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(BASE64_STANDARD.encode(&shot.jpeg), "image/jpeg"),
            ContentBlock::text(format!(
                "{}\n\nOCR text:\n{recognised}",
                shot.coordinate_note()
            )),
        ]))
    }

    #[tool(
        description = "Find text on screen and report absolute screen coordinates for each match."
    )]
    fn find_text_on_screen(
        &self,
        Parameters(request): Parameters<TextSearchRequest>,
    ) -> Result<String, ErrorData> {
        let shot = screen::capture(request.output.as_deref(), None, screen::DEFAULT_MAX_WIDTH)
            .map_err(failed)?;
        let found = screen::find_text(&shot.native_png, &request.target).map_err(failed)?;
        if found.is_empty() {
            return Ok(format!("no match for {:?} on screen", request.target));
        }
        let lines: Vec<String> = found
            .iter()
            .map(|hit| {
                format!(
                    "{:?} confidence {:.0} at screen ({}, {})",
                    hit.text,
                    hit.confidence,
                    i64::from(hit.center_x) + shot.origin_x,
                    i64::from(hit.center_y) + shot.origin_y
                )
            })
            .collect();
        Ok(lines.join("\n"))
    }

    #[tool(description = "Find text on screen and click its first match.")]
    fn click_text(
        &self,
        Parameters(request): Parameters<TextSearchRequest>,
    ) -> Result<String, ErrorData> {
        let shot = screen::capture(request.output.as_deref(), None, screen::DEFAULT_MAX_WIDTH)
            .map_err(failed)?;
        let found = screen::find_text(&shot.native_png, &request.target).map_err(failed)?;
        let best = found.first().ok_or_else(|| {
            ErrorData::internal_error(format!("no match for {:?}", request.target), None)
        })?;

        let x = i64::from(best.center_x) + shot.origin_x;
        let y = i64::from(best.center_y) + shot.origin_y;
        input::move_pointer(x as i32, y as i32).map_err(failed)?;
        input::click("left", false).map_err(failed)?;
        Ok(format!("clicked {:?} at ({x}, {y})", best.text))
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
    version = "0.1.0",
    instructions = "Inspect and control this desktop's windows, workspaces, monitors, input, screen and clipboard (Umbriel Wayland compositor).\n\nThis desktop runs Umbriel, not Hyprland, Sway, i3 or X11, so hyprctl, swaymsg, i3-msg, wmctrl and xdotool do not work here; answer questions about open windows, workspaces and monitors with these tools. Each monitor has its own numbered workspaces: list_windows with workspace \"4\" shows workspace 4 on every monitor. Window and workspace state come straight from Umbriel's IPC socket; use run_action for any compositor action without a dedicated tool."
)]
impl ServerHandler for UmbrielServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.profile.permit_tool(&request.name).map_err(refused)?;
        let tool_call = ToolCallContext::new(self, request, context);
        Self::tool_router().call(tool_call).await
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
}

#[derive(clap::Subcommand)]
enum Command {
    /// Check that Umbriel, its input protocols, screen capture, and OCR are ready
    Doctor,
    /// Register this server with an AI coding harness, or print the config to paste
    Install {
        harness: register::Harness,
        /// Restrict the registered server to this profile (default: no restriction)
        #[arg(long, value_enum)]
        profile: Option<Profile>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Doctor) => return Ok(doctor::run()),
        Some(Command::Install { harness, profile }) => return Ok(register::run(harness, profile)),
        None => {}
    }
    let service = UmbrielServer {
        profile: cli.profile,
    }
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
        let instructions = UmbrielServer {
            profile: crate::profile::Profile::Full,
        }
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
