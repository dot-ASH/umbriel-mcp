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
should. The server itself is a static binary for x86_64 and aarch64 Linux with no runtime
libraries, so the distribution only matters for installing Umbriel and the two screen
helpers.

| Distribution | Umbriel | umbriel-mcp |
|---|---|---|
| Arch Linux | AUR `umbriel-git` | AUR `umbriel-mcp-bin` |
| Ubuntu 26.04+ | Noctalia APT repository (`umbriel`) | release binary |
| Debian Trixie / Sid | Noctalia APT repository (`umbriel`) | release binary |
| Fedora | Terra (`umbriel-nightly`) | release binary |
| anything else | see [Umbriel's installation page](https://docs.noctalia.dev/umbriel/installation/) | release binary or `cargo install` |

Older Ubuntu releases are not supported because Umbriel is not packaged for them.

**Warning for Arch users:** the AUR package named `umbriel` is a placeholder and is not
the compositor. Do not install it; the compositor is `umbriel-git`.

## Install

Each path below installs Umbriel, the screen helpers, and `umbriel-mcp`, then checks the
result. Skip the Umbriel step if you already run it. The Umbriel commands are copied from
[its installation page](https://docs.noctalia.dev/umbriel/installation/); check there if
they have changed.

### Arch Linux

```sh
yay -S umbriel-git                                     # the compositor, if needed
sudo pacman -S grim tesseract tesseract-data-eng       # screen capture and OCR
yay -S umbriel-mcp-bin                                 # this server, prebuilt
umbriel-mcp doctor
```

### Ubuntu 26.04 and newer

1. Install Umbriel from the Noctalia APT repository (amd64 and arm64 only), if needed:

   ```sh
   wget https://pkg.noctalia.dev/deb/nickh-archive-keyring.deb
   sudo dpkg -i nickh-archive-keyring.deb
   sudo wget -O /etc/apt/sources.list.d/noctalia-resolute.sources \
     https://pkg.noctalia.dev/deb/noctalia-resolute.sources
   sudo apt update
   sudo apt install umbriel
   ```

2. Install the screen helpers:

   ```sh
   sudo apt install grim tesseract-ocr tesseract-ocr-eng
   ```

3. Install the server from the latest [GitHub release](https://github.com/dot-ASH/umbriel-mcp/releases):

   ```sh
   arch=$(uname -m)   # x86_64 or aarch64
   tag=v0.1.0
   base="https://github.com/dot-ASH/umbriel-mcp/releases/download/$tag"
   curl -LO "$base/umbriel-mcp-$tag-$arch-linux.tar.gz"
   curl -LO "$base/SHA256SUMS"
   sha256sum --check --ignore-missing SHA256SUMS
   tar -xzf "umbriel-mcp-$tag-$arch-linux.tar.gz"
   sudo install -Dm755 "umbriel-mcp-$tag-$arch-linux/umbriel-mcp" /usr/local/bin/umbriel-mcp
   ```

   `/usr/local/bin` is on every user's `PATH`, so harnesses started from the Umbriel session
   find the binary without extra setup. To install without `sudo`, use
   `~/.local/bin/umbriel-mcp` instead; Ubuntu adds `~/.local/bin` to `PATH` at login only if
   the directory already existed, so log out and back in after creating it.

4. Check the setup from a terminal inside the Umbriel session:

   ```sh
   umbriel-mcp doctor
   ```

### Debian Trixie and Sid

Same as Ubuntu, with the Debian repository source and, on Trixie, the backports suite:

```sh
wget https://pkg.noctalia.dev/deb/nickh-archive-keyring.deb
sudo dpkg -i nickh-archive-keyring.deb
sudo wget -O /etc/apt/sources.list.d/noctalia-trixie.sources \
  https://pkg.noctalia.dev/deb/noctalia-trixie.sources        # Sid: noctalia-unstable.sources
sudo apt update
sudo apt install -t trixie-backports umbriel                  # Sid: sudo apt install umbriel
sudo apt install grim tesseract-ocr tesseract-ocr-eng
```

Then install the server from the release binary as in step 3 above and run
`umbriel-mcp doctor`.

### Fedora

```sh
sudo dnf install umbriel-nightly                              # from the Terra repository
sudo dnf install grim tesseract tesseract-langpack-eng
```

Then install the server from the release binary as in Ubuntu step 3 and run
`umbriel-mcp doctor`.

### Other distributions

Install Umbriel following [its installation page](https://docs.noctalia.dev/umbriel/installation/),
then `grim`, `tesseract`, and Tesseract's English data (`eng.traineddata`) from your package
manager. Install the server from the release binary as in Ubuntu step 3, or build it with
Cargo (needs Rust 1.89 or newer, for example from [rustup](https://rustup.rs)):

```sh
cargo install umbriel-mcp --locked
```

`umbriel-mcp doctor` names the exact packages to install on Arch, Debian/Ubuntu, and Fedora,
and gives generic guidance elsewhere.

## Requirements

Pointer, keyboard, and clipboard need nothing beyond a running Umbriel session. These
external programs are needed only for the screen tools:

| Tool | Used for | Arch | Debian/Ubuntu | Fedora |
|---|---|---|---|---|
| `grim` | `screenshot`, OCR tools | `grim` | `grim` | `grim` |
| `tesseract` | `screenshot_with_ocr`, `find_text_on_screen`, `click_text` | `tesseract` | `tesseract-ocr` | `tesseract` |
| English OCR data | same | `tesseract-data-eng` | `tesseract-ocr-eng` | `tesseract-langpack-eng` |

The tools are always listed; a call that needs a missing program fails with an error naming
the package to install.

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
registration uses direct exposure and a description, so the model sees the tools in its tool
list and answers "what's on workspace 4?" with one `list_windows` call instead of searching
for tools or trying `hyprctl`. If you registered by hand earlier, run `umbriel-mcp install pi`
again to switch to direct exposure. The Zed snippet keeps `"args": []` because Zed rejects its whole settings file
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

**About Umbriel itself** — `umbriel_info` (version, keyboard layouts, submap, color and
tearing state), `list_actions` (every action `run_action` and keybinds accept, filterable),
`config_options` (exact config keys with types, defaults and allowed values, from Umbriel's own
schema), `read_config` (your config or one section such as `keybinds`, plus validation). These
read from the installed `umbriel` command and your config, so they match the version you run.
The schema has no prose descriptions; explanations live in the
[Umbriel documentation](https://docs.noctalia.dev/umbriel/).

**Screen and clipboard** — `screenshot`, `screenshot_with_ocr`, `find_text_on_screen`,
`click_text`, `clipboard_read`, `clipboard_write`. With a `window_id`, `screenshot` and
`screenshot_with_ocr` capture that window even on a hidden workspace, without switching to it
(needs `grim` 1.5 or newer; older `grim` can only capture windows that are on screen).

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
