# touchbar-daemon

A Touch Bar daemon for Linux on Intel T2 MacBook Pros, with **expandable
button groups** and any number of layers.

Written from scratch on top of the kernel's `appletbdrm` display driver,
evdev touch input and uinput. It does not use code from tiny-dfr.

- Buttons with icons (any installed icon theme, or your own SVG/PNG),
  text, a clock, or the battery level
- Keys and key combos (`["LeftCtrl", "C"]`)
- `open = "layer"` expands a group of buttons over the bar with a back
  button, and it collapses again after a timeout
- `switch = "layer"` changes to another layer
- A separate layer while Fn is held
- Dims and turns off when idle, wakes on touch
- Reloads its config when the file changes

## Config

See [`data/config.toml`](data/config.toml); every option is documented
there. Copy it to `/etc/touchbar-daemon/config.toml` to customise it.

Preview a layer without root or a Touch Bar:

```
touchbar-daemon --preview out.png --layer media
touchbar-daemon --check --config my-config.toml
```

## Install

```
cargo build --release
sudo install -Dm755 target/release/touchbar-daemon /usr/bin/touchbar-daemon
sudo install -Dm644 data/config.toml /etc/touchbar-daemon/config.toml
sudo install -Dm644 data/touchbar-daemon.service /etc/systemd/system/touchbar-daemon.service
sudo install -Dm644 data/90-touchbar-daemon.rules /etc/udev/rules.d/90-touchbar-daemon.rules
sudo systemctl disable --now tiny-dfr.service 2>/dev/null
sudo systemctl mask tiny-dfr.service   # its udev rule would start it again at boot
sudo systemctl enable --now touchbar-daemon.service
```

The service conflicts with tiny-dfr, since only one program can drive the
bar. To go back:

```
sudo systemctl disable --now touchbar-daemon
sudo systemctl unmask tiny-dfr
sudo systemctl start tiny-dfr
```

## Status

Early. Tested on a MacBookPro16,1 (T2). Apple Silicon is untested.

## License

MIT
