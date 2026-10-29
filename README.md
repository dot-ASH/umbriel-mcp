# umbriel-mcp

An MCP server for the [Umbriel](https://docs.noctalia.dev/umbriel/) Wayland compositor.

Exposes Umbriel to MCP clients: list and control windows, workspaces and outputs, drive
the pointer and keyboard, capture the screen with optional OCR, and read or write the
clipboard.

Compositor state and actions go straight to Umbriel's IPC socket rather than shelling out
to the CLI, so inspection costs one round trip and no process spawn. Input, screen
capture, OCR, and clipboard delegate to the usual Wayland-generic helpers.

## Links

- Source: <https://github.com/dot-ASH/umbriel-mcp>
- MCP Registry name: `mcp-name: io.github.dot-ash/umbriel-mcp`

## Install

From crates.io:

```sh
cargo install umbriel-mcp
```

That puts `umbriel-mcp` in `~/.cargo/bin`. From source instead:

```sh
git clone https://github.com/dot-ASH/umbriel-mcp
cd umbriel-mcp
cargo build --release
install -Dm755 target/release/umbriel-mcp ~/.local/bin/umbriel-mcp
```

## Requirements

| Tool | Used for | Arch package |
|---|---|---|
| Umbriel | compositor IPC | `umbriel-git` |
| `grim` | screen capture | `grim` |
| `wl-clipboard` | clipboard read and write | `wl-clipboard` |
| `tesseract` + English data | OCR tools only | `tesseract`, `tesseract-data-eng` |

Pointer and keyboard input use Umbriel's Wayland virtual-pointer and virtual-keyboard
protocols directly, so no input daemon, device permissions, or privileged group are needed.
Only the screen-capture, clipboard, and OCR tools depend on the helpers above.

The socket is found through `UMBRIEL_SOCKET`, falling back to
`$XDG_RUNTIME_DIR/umbriel-$WAYLAND_DISPLAY.sock`.

## Configure

Register it as a stdio server with your MCP client:

```json
{
  "mcpServers": {
    "umbriel": {
      "type": "stdio",
      "command": "umbriel-mcp"
    }
  }
}
```

## Tools

**Inspection** — `list_windows`, `list_workspaces`, `list_outputs`, `get_active_window`,
`list_layers`

**Window and workspace control** — `switch_workspace`, `focus_window`, `close_window`,
`kill_window`, `move_window_to_workspace`, `toggle_fullscreen`, `toggle_floating`,
`resize_window`, `set_workspace_layout`, `toggle_overview`, `launch_app`, `run_action`

**Input** — `mouse_move`, `mouse_click`, `mouse_scroll`, `mouse_drag`, `type_text`,
`key_press`

**Screen and clipboard** — `screenshot`, `screenshot_with_ocr`, `find_text_on_screen`,
`click_text`, `clipboard_read`, `clipboard_write`

`run_action` accepts any action from `umbriel msg --help`, so the compositor's full verb
set stays reachable even where there is no dedicated tool.

## Notes

Workspaces in Umbriel belong to an output rather than to a global pool. Selectors take a
position or a name and accept an optional `/output` suffix, which the `workspace` and
`output` arguments build for you.

Screenshots are downscaled to 1024px wide by default and returned as JPEG alongside a
coordinate-mapping note, so screen coordinates can be recovered from image coordinates.
`find_text_on_screen` and `click_text` apply that mapping themselves and report absolute
screen coordinates.

## License

MIT
