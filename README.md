# Oma-Touchbar

A Touch Bar daemon and settings app for Linux on Intel T2 MacBook Pros,
with **expandable button groups** and any number of layers.

Written in Rust from scratch, on top of the kernel's `appletbdrm` display
driver, evdev touch input and uinput. It does not use code from tiny-dfr.

| Part | What it is |
|---|---|
| `daemon/` | `touchbar-daemon`: draws the buttons and sends their keys |
| `settings/` | `touchbar-settings`: a GTK 4 / libadwaita app to edit the layout |

## Features

- Buttons with icons (any installed icon theme, or your own SVG/PNG),
  text, a clock, or the battery level
- Keys and key combos (`["LeftCtrl", "C"]`)
- `open = "layer"` expands a group of buttons over the bar with a back
  button; it collapses again after a timeout
- `switch = "layer"` changes to another layer
- A separate layer while Fn is held
- Dims and turns off when idle, wakes on touch
- Reloads its config as soon as the file changes

The settings app shows a live preview drawn by the daemon's own renderer,
lets you create groups, add/reorder/edit buttons with an icon and key
picker, manage layers, and saves automatically (with undo).

## Build

Needs Rust, GTK 4.12+ and libadwaita 1.5+ (for the settings app).

```
cargo build --release
```

## Install

```
sudo install -Dm755 target/release/touchbar-daemon /usr/bin/touchbar-daemon
sudo install -Dm755 target/release/touchbar-settings /usr/bin/touchbar-settings
sudo install -Dm644 daemon/data/config.toml /etc/touchbar-daemon/config.toml
sudo install -Dm644 daemon/data/touchbar-daemon.service /etc/systemd/system/touchbar-daemon.service
sudo install -Dm644 daemon/data/90-touchbar-daemon.rules /etc/udev/rules.d/90-touchbar-daemon.rules
sudo install -Dm644 settings/data/io.github.cyhunterx.OmaTouchbar.desktop /usr/share/applications/io.github.cyhunterx.OmaTouchbar.desktop
sudo systemctl disable --now tiny-dfr.service 2>/dev/null
sudo systemctl mask tiny-dfr.service   # its udev rule would start it again at boot
sudo systemctl enable --now touchbar-daemon.service
```

The first time the settings app saves, it asks once for your password to
let your user edit `/etc/touchbar-daemon/`; after that it saves without
prompts.

Only one program can drive the bar, so the service conflicts with tiny-dfr.
To go back:

```
sudo systemctl disable --now touchbar-daemon
sudo systemctl unmask tiny-dfr
sudo systemctl start tiny-dfr
```

## Config

Every option is documented in [`daemon/data/config.toml`](daemon/data/config.toml).
You can edit `/etc/touchbar-daemon/config.toml` by hand instead of using
the app. Useful commands:

```
touchbar-daemon --check --config my-config.toml
touchbar-daemon --preview out.png --layer media      # no root or Touch Bar needed
TOUCHBAR_CONFIG=/tmp/test.toml touchbar-settings      # edit a test file
```

## Status

Early. Tested on a MacBookPro16,1 (T2). Apple Silicon is untested.

## License

MIT
