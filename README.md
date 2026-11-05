# umbriel-mcp

mcp-name: io.github.dot-ASH/umbriel-mcp

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
| Arch Linux | AUR `umbriel-git` | `makepkg` from this repo (AUR package planned) |
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
sudo pacman -S grim tesseract tesseract-data-eng wf-recorder   # screenshots, OCR, recording
```

Then install the server as a pacman package built from this repository's recipe. It downloads
the prebuilt release binary and checks its checksum, so nothing is compiled:

```sh
git clone https://github.com/dot-ASH/umbriel-mcp
cd umbriel-mcp/packaging/aur-bin
makepkg -si
umbriel-mcp doctor
```

Remove it later with `sudo pacman -R umbriel-mcp-bin`. An AUR package of the same name is
planned; this section will switch to `yay -S umbriel-mcp-bin` once it is published.

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
   sudo apt install grim tesseract-ocr tesseract-ocr-eng wf-recorder
   ```

3. Install the server:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/dot-ASH/umbriel-mcp/main/install.sh | sh
   ```

   This downloads the latest release for your architecture, checks it against the release's
   `SHA256SUMS`, installs it to `~/.local/bin/umbriel-mcp` (no `sudo`), and runs
   `umbriel-mcp doctor`. Ubuntu adds `~/.local/bin` to `PATH` at login once the folder exists,
   so log out and back in if `umbriel-mcp` is not found afterwards. If you installed an older
   version into `/usr/local/bin`, the script tells you to remove it.

   <details><summary>Manual install without the script</summary>

   ```sh
   arch=$(uname -m)   # x86_64 or aarch64
   tag=v0.4.1
   base="https://github.com/dot-ASH/umbriel-mcp/releases/download/$tag"
   curl -LO "$base/umbriel-mcp-$tag-$arch-linux.tar.gz"
   curl -LO "$base/SHA256SUMS"
   sha256sum --check --ignore-missing SHA256SUMS
   tar -xzf "umbriel-mcp-$tag-$arch-linux.tar.gz"
   install -Dm755 "umbriel-mcp-$tag-$arch-linux/umbriel-mcp" ~/.local/bin/umbriel-mcp
   ```

   </details>

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
sudo apt install grim tesseract-ocr tesseract-ocr-eng wf-recorder
```

Then install the server, which also runs `umbriel-mcp doctor`:

```sh
curl -fsSL https://raw.githubusercontent.com/dot-ASH/umbriel-mcp/main/install.sh | sh
```

### Fedora

```sh
sudo dnf install umbriel-nightly                              # from the Terra repository
sudo dnf install grim tesseract tesseract-langpack-eng wf-recorder
```

Then install the server, which also runs `umbriel-mcp doctor`:

```sh
curl -fsSL https://raw.githubusercontent.com/dot-ASH/umbriel-mcp/main/install.sh | sh
```

### Other distributions

Install Umbriel following [its installation page](https://docs.noctalia.dev/umbriel/installation/),
then `grim`, `tesseract`, and Tesseract's English data (`eng.traineddata`) from your package
manager. Install the server with `curl -fsSL https://raw.githubusercontent.com/dot-ASH/umbriel-mcp/main/install.sh | sh`, or build it with
Cargo (needs Rust 1.89 or newer, for example from [rustup](https://rustup.rs)):

```sh
cargo install umbriel-mcp --locked
```

`umbriel-mcp doctor` names the exact packages to install on Arch, Debian/Ubuntu, and Fedora,
and gives generic guidance elsewhere.

## Updating

```sh
umbriel-mcp update            # install the latest release in place
umbriel-mcp update --check    # only report whether one exists
```

`update` downloads the latest release, checks it against `SHA256SUMS`, confirms the new
binary runs and reports the expected version, and only then replaces the old one, so a failed
update leaves the old binary working. Restart your harness afterwards; running sessions keep
the old version. `umbriel-mcp doctor` also says when a newer version exists. Nothing checks for
updates in the background.

- If the binary is in a folder you cannot write to, such as `/usr/local/bin`, it asks you to run
  `sudo umbriel-mcp update`.
- If it was installed by a package manager (pacman or makepkg, `cargo install`, Nix), it refuses
  and tells you the package manager's command instead, so the two never disagree.
- Versions before 0.3.0 cannot update themselves to a signed release; run the install script
  once to get 0.3.0, and `umbriel-mcp update` works from then on.
- Releases are signed. `update` and the install script accept a release only if its
  `SHA256SUMS` carries a valid signature from the umbriel-mcp release key, whose public half is
  built into the binary and the script, and only then trust its checksums. A tampered or unsigned
  release is refused. Updates are protected even if the GitHub account were compromised; a first
  install with the script is not, because the script itself comes from GitHub.
- Releases 0.2.3 and older are unsigned, so the script and `update` only install 0.3.0 and newer.

## Uninstalling

```sh
umbriel-mcp uninstall
```

This lists what it will remove and asks once (`--yes` skips the question):

- the `umbriel` server registered with pi, Claude Code, opencode, Cursor, VS Code or Zed.
  Claude Code is unregistered through its own CLI; in the others' config files only the
  `umbriel` entry is deleted, and comments, formatting and other servers stay as they were. An
  entry named `umbriel` that runs some other program is left alone.
- the `umbriel-mcp` program itself, when the install script or `umbriel-mcp update` put it
  there. If you run it through a symlink, only the link is removed. For a pacman, cargo or Nix
  install it prints the command to run instead.

`--keep-binary` only unregisters. Config backups and screen recordings are listed but kept.
Restart any running harness afterwards. Registrations inside a single project (a project's
`opencode.json` or `.cursor/mcp.json`) are not searched; delete those entries by hand.

## Requirements

Pointer, keyboard, and clipboard need nothing beyond a running Umbriel session. These
external programs are needed only for the screen tools:

| Tool | Used for | Arch | Debian/Ubuntu | Fedora |
|---|---|---|---|---|
| `grim` | `screenshot`, OCR tools | `grim` | `grim` | `grim` |
| `tesseract` | `screenshot_with_ocr`, `find_text_on_screen`, `click_text` | `tesseract` | `tesseract-ocr` | `tesseract` |
| English OCR data | same | `tesseract-data-eng` | `tesseract-ocr-eng` | `tesseract-langpack-eng` |
| `wf-recorder` | `start_recording`, `stop_recording` | `wf-recorder` | `wf-recorder` | `wf-recorder` |

The tools are always listed; a call that needs a missing program fails with an error naming
the package to install.

The socket is found through `UMBRIEL_SOCKET`, falling back to
`$XDG_RUNTIME_DIR/umbriel-$WAYLAND_DISPLAY.sock`.

## Register with your harness

```sh
umbriel-mcp install pi            # also: claude-code, vscode, opencode, cursor
umbriel-mcp install zed           # prints config to paste
umbriel-mcp install claude-code --profile no-exec
```

This registers the server through each harness's own CLI, so umbriel-mcp never reads or
edits their config files itself:

| Harness | Runs |
|---|---|
| pi | `pi mcp add` with direct exposure and a description |
| Claude Code | `claude mcp add --scope user` |
| VS Code | `code --add-mcp` |
| opencode | `opencode mcp add` |
| Cursor | `cursor-agent mcp add`, if your `cursor-agent` has it |
| Zed | nothing: Zed has no CLI for this, so the config to paste is printed |

If opencode's or Cursor's CLI is not installed, or Cursor's has no `mcp add`, the config to
paste is printed instead. A profile is added to the registration only if you pass
`--profile`, so the default stays `full`.

The pi registration uses direct exposure, so the model sees the tools in its tool list and
answers "what's on workspace 4?" with one `list_windows` call instead of searching for tools or
trying `hyprctl`. If you registered pi by hand earlier, run `umbriel-mcp install pi` again to
switch to direct exposure. The Zed snippet keeps `"args": []` because Zed rejects its whole
settings file if it is missing.

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
Tesseract's English data and `wf-recorder` are installed. Each problem is listed on its own with the command to
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
`list_layers`. Window and monitor listings are brief by default: id, app, title, place, size,
workspace and `has_keyboard_focus` for windows; name, position, size, current mode, scale and
workspace for monitors. Pass `detail: true` for every field Umbriel reports, including every
display mode.

**Window and workspace control** — `switch_workspace`, `focus_window`, `close_window`,
`kill_window`, `move_window_to_workspace`, `toggle_fullscreen`, `toggle_floating`,
`resize_window`, `set_workspace_layout`, `toggle_overview`, `launch_app`, `run_action`

**Input** — `mouse_move`, `mouse_click`, `mouse_scroll`, `mouse_drag`, `type_text`,
`key_press`. These drive your real mouse and keyboard, so they collide with you if you use the
desktop at the same time; the server tells agents to ask before taking over. To keep input
where it was meant to go:

- `type_text` and `key_press` take a `window_id`: if that window does not have keyboard focus,
  nothing is typed and the error names the window that does.
- `mouse_click` and `mouse_move` take a `window_id` with `x` and `y` counted from that window's
  top-left corner. A window that is not on screen, or a point outside it, is refused.
- After typing, pressing or clicking, the result says which window has keyboard focus.

**About Umbriel itself** — `umbriel_info` (version, keyboard layouts, submap, color and
tearing state), `list_actions` (every action `run_action` and keybinds accept, filterable),
`config_options` (exact config keys with types, defaults and allowed values, from Umbriel's own
schema), `read_config` (your config or one section such as `keybinds`, plus validation). These
read from the installed `umbriel` command and your config, so they match the version you run.
The schema has no prose descriptions; explanations live in the
[Umbriel documentation](https://docs.noctalia.dev/umbriel/).

**Changing settings** — `set_option` and `reset_option` change one setting, such as
`layout.gap` or `appearance.blur.enabled`, and `set_keybind` and `remove_keybind` change
keybinds. Keys and values are checked against Umbriel's schema (type, range, allowed values),
then the edited config is checked with `umbriel config validate`. Rule lists such as
`window_rule` are edited with `add_rule`, `replace_rule` and `remove_rule`, which take a
rule as JSON and a position counted from 1 in the order `read_config` shows.

**Every change needs your approval before anything is written:**

- In clients that support MCP elicitation (Claude Code 2.1.76+, VS Code, Cursor, opencode v2),
  you get a dialog showing the exact change, with a checkbox for keeping a backup. Only your approval writes it; the model cannot
  answer the dialog. No answer within 10 minutes writes nothing.
- In clients without it (pi, opencode 1.x), the tool writes nothing and returns the change with
  a one-time confirmation code. The model is told to show you the change, ask whether to keep a
  backup, and wait for your answer before calling `apply_config_change`. This relies on the model following that instruction,
  so it is weaker than a dialog. All write tools are marked destructive, so a harness
  permission prompt or extension can ask you as well.
- A code expires after 10 minutes, works once, and is refused if the file changed since the
  preview.

The same approval step asks whether to keep a backup of the previous config. If you say yes,
or don't answer that question, it is saved next to the config as `config.toml.bak-<time>`; say
no to avoid collecting backup files. umbriel-mcp keeps the newest 5
of its own backups and deletes older ones whenever it writes a new one; start it with
`--keep-backups N` to keep more (up to 1000). When a change is
written, only the changed lines differ (comments and formatting are kept), a symlinked config
stays a symlink, and Umbriel applies it immediately.

Profiles still apply. `read-only` refuses all changes. `no-exec` refuses `spawn:` keybinds and
hot-corner actions, and settings that can start programs, load files, or weaken a security
check (`general.autostart`, `events`, `environment`, `include`, effect shaders, `drm`,
`screencast.disable_dynamic_confirmation`); those need `full`. Disabling your only enabled
output is always refused, and keyboard-layout or `general.mod_key` changes come with a warning.

**Recording** — `start_recording` records one monitor, or the screen area of one window that
is on screen, to `~/Videos/umbriel-mcp/recording-<UTC time>.mp4` (or your `XDG_VIDEOS_DIR`).
Audio is off unless asked for. A recording stops by itself after 30 seconds by default (1 second
to 60 minutes allowed), `stop_recording` stops it sooner and reports the file, and only one runs
at a time.
If umbriel-mcp exits, the recording is stopped and the file finished. Recording is observation
like screenshots, so every profile allows it. `wf-recorder` shows nothing on screen while it
records.

**Choosing what to capture** — when you ask for a screenshot or a recording without saying what
to capture, you are asked to choose: the whole screen (screenshots only), a monitor, or a
window. For a recording you are also asked how long, from 30 seconds to 60 minutes, with 30
seconds preselected, unless you already said how long. Clients with dialogs show the choices in one; others have the model ask you. When the
model takes a screenshot or starts a recording for its own purposes, it does not ask and never
waits for you.

**Screen and clipboard** — `screenshot`, `screenshot_with_ocr`, `find_text_on_screen`,
`click_text`, `wait_for_text`, `wait_for_change`, `clipboard_read`, `clipboard_write`.

- With a `window_id`, captures and text search cover only that window, even on a hidden
  workspace, without switching to it (needs `grim` 1.5 or newer; older `grim` can only capture
  windows that are on screen). This leaves your other windows out of the picture.
- With a `region` (`x`, `y`, `width`, `height` in screen coordinates), they cover only that
  rectangle, at full resolution, to zoom in on a detail.
- Every capture says when it was taken, and notes when a window's title changed since its last
  capture, which usually means it shows a different page or someone else is using it.
- `wait_for_text` waits until text appears. `wait_for_change` waits until a window, monitor or
  region differs from your previous capture of it and then stops changing, and returns the
  settled picture; if nothing changed, it says so. Both give up after 10 seconds by default
  (30 at most), so an agent can wait for a page to load instead of guessing with sleeps.

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
