# umbriel-mcp

mcp-name: io.github.dot-ash/umbriel-mcp

An MCP server for the [Umbriel](https://docs.noctalia.dev/umbriel/) Wayland compositor.

Exposes Umbriel to MCP clients: list and control windows, workspaces and outputs, drive
the pointer and keyboard, capture the screen with optional OCR, and read or write the
clipboard.

Compositor state and actions go straight to Umbriel's IPC socket rather than shelling out
to the CLI, so inspection costs one round trip and no process spawn. Pointer, keyboard, and
clipboard use Wayland protocols directly. Only screen capture and OCR call external programs.

## Read this before installing

**Full access means full control of your desktop.** In the default `full` profile an agent
connected to this server can launch arbitrary commands, type and click anywhere, read and
overwrite your clipboard, and see your screen. Anything on screen or in the clipboard is
visible to the model. Use the [profiles](#profiles) to grant less.

This is a best-effort personal project that tracks an actively evolving compositor. Expect
0.x behavior changes, and expect it to break when Umbriel changes. There is no support
promise beyond what I can do in my spare time.

## Compatibility

Support follows Umbriel's own availability: if Umbriel runs on your system, this server
should. Umbriel is packaged for Arch Linux (`umbriel-git`); on Ubuntu, 26.04 and newer is
the supported path. Follow [Umbriel's installation page](https://docs.noctalia.dev/umbriel/)
for your distribution.

**Warning for Arch users:** the AUR package named `umbriel` is a placeholder and is not
the compositor. Do not install it; the compositor is `umbriel-git`.

## Install

Pick one. All three give you an `umbriel-mcp` executable.

**Prebuilt binary (x86_64 and aarch64 Linux).** Static binaries with no runtime
libraries, attached to each [GitHub release](https://github.com/dot-ASH/umbriel-mcp/releases):

```sh
arch=$(uname -m)   # x86_64 or aarch64
tag=v0.1.0
curl -LO "https://github.com/dot-ASH/umbriel-mcp/releases/download/$tag/umbriel-mcp-$tag-$arch-linux.tar.gz"
curl -LO "https://github.com/dot-ASH/umbriel-mcp/releases/download/$tag/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "umbriel-mcp-$tag-$arch-linux.tar.gz"
install -Dm755 "umbriel-mcp-$tag-$arch-linux/umbriel-mcp" ~/.local/bin/umbriel-mcp
```

**Arch Linux (AUR):** `umbriel-mcp-bin` installs the same release binary without compiling.

```sh
yay -S umbriel-mcp-bin
```

**crates.io** (needs Rust 1.89 or newer):

```sh
cargo install umbriel-mcp --locked
```

That puts `umbriel-mcp` in `~/.cargo/bin`.

## Requirements

Pointer, keyboard, and clipboard need nothing beyond a running Umbriel session. These
external programs are needed only for the screen tools:

| Tool | Used for | Arch | Debian/Ubuntu | Fedora |
|---|---|---|---|---|
| `grim` | `screenshot`, OCR tools | `grim` | `grim` | `grim` |
| `tesseract` | `screenshot_with_ocr`, `find_text_on_screen`, `click_text` | `tesseract` | `tesseract-ocr` | `tesseract` |
| English OCR data | same | `tesseract-data-eng` | `tesseract-ocr-eng` | `tesseract-langpack-eng` |

The tools are always listed; a call that needs a missing program fails with an error naming
it. Run `umbriel-mcp doctor` (below) to find out what is missing.

The socket is found through `UMBRIEL_SOCKET`, falling back to
`$XDG_RUNTIME_DIR/umbriel-$WAYLAND_DISPLAY.sock`.

## Register with your harness

```sh
umbriel-mcp install pi            # also: claude-code, vscode
umbriel-mcp install opencode      # also: cursor, zed (prints config to paste)
umbriel-mcp install claude-code --profile no-exec
```

For `pi`, `claude-code`, and `vscode` this runs the harness's own CLI (`pi mcp add`,
`claude mcp add`, `code --add-mcp`). For `opencode`, `cursor`, and `zed` it prints the
configuration to paste; it never reads or edits your config files. A profile is added to
the registration only if you pass `--profile`, so the default stays `full`. The pi
registration uses direct exposure so the tools are offered to the model without codemode
discovery. The Zed snippet keeps `"args": []` because Zed rejects its whole settings file
if it is missing.

Any other MCP client can use the plain stdio form:

```json
{
  "mcpServers": {
    "umbriel": { "command": "umbriel-mcp", "args": [] }
  }
}
```

## Checking your setup

`umbriel-mcp doctor` verifies that the Umbriel socket is reachable, that the compositor
offers the virtual-pointer and virtual-keyboard protocols, and that `grim`, `tesseract`, and
Tesseract's English data are installed. Each problem is listed on its own with the command to
fix it on Arch, Debian/Ubuntu, and Fedora, or generic advice on other distributions. It exits
with status 0 only when everything is ready.

## Profiles

`--profile` limits what the server will do. Every tool stays visible in every profile; a
call the profile does not allow returns an error that names the profile.

| Profile | Allows |
|---|---|
| `full` (default) | everything, including `launch_app` and spawn actions |
| `no-exec` | everything except starting programs: `launch_app` and any `run_action` that spawns are refused |
| `read-only` | queries, screenshots, OCR, and reading the clipboard |

`no-exec` stops the server from starting programs; it is not a sandbox. An agent that can
type into a focused terminal can still run commands through the input tools.

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

`key_press` takes chords such as `ctrl+shift+Tab`, `super+q`, `F5` or `alt+Left`; an uppercase
letter (`ctrl+A`) implies Shift. `type_text` accepts any Unicode text, with `\n` typed as Return
and `\t` as Tab.

`run_action` accepts any action from `umbriel msg --help`, so the compositor's full verb
set stays reachable even where there is no dedicated tool.

## Notes

Workspaces in Umbriel belong to an output rather than to a global pool. Selectors take a
position or a name and accept an optional `/output` suffix, which the `workspace` and
`output` arguments build for you.

Screenshots are downscaled to 1024px wide by default and returned as JPEG alongside a
coordinate-mapping note, so screen coordinates can be recovered from image coordinates.
OCR is different: it always runs on the full-resolution capture, because recognising text in
the downscaled preview finds nothing. `find_text_on_screen` and `click_text` therefore report
absolute screen coordinates directly, and match multi-word phrases such as `Open File`.

## License

MIT
