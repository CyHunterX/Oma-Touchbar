mod config;
mod display;
mod input;
mod render;

use anyhow::{Context, Result, bail};
use config::Config;
use evdev::KeyCode;
use render::{Icons, Slot};
use std::{
    fs,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

const USAGE: &str = "\
touchbar-daemon: draws buttons on the Touch Bar and sends their keys

Usage:
  touchbar-daemon [--config PATH]
  touchbar-daemon --check [--config PATH]
  touchbar-daemon --preview OUT.png [--layer NAME] [--width N] [--config PATH]
  touchbar-daemon --print-default-config

--check     validate the config and exit
--preview   render a layer to a PNG instead of the Touch Bar (no root needed)
";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Which layers are on screen.
#[derive(Debug, Clone, PartialEq)]
struct View {
    /// The current layer, changed by `switch` buttons.
    base: String,
    /// Layers expanded with `open`, most recent last.
    opened: Vec<String>,
    fn_held: bool,
}

impl View {
    fn new(config: &Config) -> View {
        View {
            base: config.general.default_layer.clone(),
            opened: Vec::new(),
            fn_held: false,
        }
    }

    fn showing_fn(&self, config: &Config) -> bool {
        self.fn_held && !config.general.fn_layer.is_empty()
    }

    fn slots(&self, config: &Config) -> Vec<Slot> {
        let mut slots = Vec::new();
        if config.general.esc_button {
            slots.push(Slot::Esc);
        }
        let name = if self.showing_fn(config) {
            &config.general.fn_layer
        } else if let Some(top) = self.opened.last() {
            slots.push(Slot::Back);
            top
        } else {
            &self.base
        };
        if let Some(layer) = config.layer(name) {
            slots.extend(layer.buttons.iter().cloned().map(Slot::Button));
        }
        slots
    }

    /// Drops references to layers that no longer exist after a reload.
    fn repair(&mut self, config: &Config) {
        if config.layer(&self.base).is_none() {
            self.base = config.general.default_layer.clone();
        }
        self.opened.retain(|l| config.layer(l).is_some());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_path = PathBuf::from(config::DEFAULT_PATH);
    let mut preview: Option<PathBuf> = None;
    let mut layer: Option<String> = None;
    let mut width = 2008;
    let mut check = false;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n\n{USAGE}");
                std::process::exit(2)
            })
        };
        match arg.as_str() {
            "--config" => config_path = value("--config").into(),
            "--preview" => preview = Some(value("--preview").into()),
            "--layer" => layer = Some(value("--layer")),
            "--width" => width = value("--width").parse().unwrap_or(2008),
            "--check" => check = true,
            "--print-default-config" => {
                print!("{}", config::DEFAULT_CONFIG);
                return;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let result = if check {
        Config::load(&config_path).map(|_| println!("{}: OK", config_path.display()))
    } else if let Some(out) = preview {
        preview_png(&config_path, &out, layer.as_deref(), width)
    } else {
        run(&config_path)
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn preview_png(config_path: &Path, out: &Path, layer: Option<&str>, width: i32) -> Result<()> {
    let config = Config::load(config_path)?;
    let mut view = View::new(&config);
    match layer {
        None => {}
        Some(name) if config.layer(name).is_none() => bail!("no layer named \"{name}\""),
        Some(name) if name == config.general.fn_layer => view.fn_held = true,
        Some(name) if name == view.base => {}
        Some(name) => view.opened.push(name.to_string()),
    }
    let surface = render::render(&config.general, &view.slots(&config), None, width, &mut Icons::default())?;
    let mut file = fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    surface.write_to_png(&mut file)?;
    println!("wrote {}", out.display());
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Power {
    On,
    Dim,
    Off,
}

/// What a finger currently on the bar is doing.
#[derive(Clone, Default)]
struct Press {
    /// Index of the slot it went down on, while it stays on it.
    slot: Option<usize>,
    /// Keys being held for it.
    keys: Vec<KeyCode>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn run(config_path: &Path) -> Result<()> {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }

    let mut config = Config::load(config_path)?;
    let mut config_mtime = mtime(config_path);

    display::ensure_display_mode();
    let mut display = display::Display::open(config.general.flip_display, Duration::from_secs(15))?;
    let mut touch = input::TouchBar::open(config.general.flip_touch, Duration::from_secs(15))?;
    let mut fn_keyboards = input::fn_keyboards();
    let mut keyboard = input::Keyboard::new()?;
    let mut backlight = display::Backlight::open();
    if backlight.is_none() {
        eprintln!("no Touch Bar backlight found; dimming disabled");
    }
    eprintln!(
        "running: {}x{} display, {} Fn keyboard(s)",
        display.width,
        display.height,
        fn_keyboards.len()
    );

    let mut icons = Icons::default();
    let mut view = View::new(&config);
    let mut presses: Vec<Press> = vec![Press::default(); 10];
    let mut last_touch = Instant::now();
    let mut power = Power::On;

    while !STOP.load(Ordering::SeqCst) {
        let slots = view.slots(&config);
        let extents = render::layout(&slots, display.width);
        let pressed = presses.iter().rev().find_map(|p| p.slot);
        let mut frame = render::render(&config.general, &slots, pressed, display.width, &mut icons)?;
        display.show(&mut frame)?;

        if let Some(bl) = backlight.as_mut() {
            bl.set(match power {
                Power::On => bl.max,
                Power::Dim => bl.max.div_ceil(2).min(bl.max.saturating_sub(1)).max(1),
                Power::Off => 0,
            });
        }

        // Sleep until input arrives or the next second starts (for the clock).
        let mut fds: Vec<libc::pollfd> = std::iter::once(touch.device.as_raw_fd())
            .chain(fn_keyboards.iter().map(|k| k.as_raw_fd()))
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_millis())
            .unwrap_or(0);
        let timeout = (1000 - now_ms as i32).clamp(10, 1000);
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

        // Fn key.
        let before = view.clone();
        for kb in fn_keyboards.iter_mut() {
            if let Some(held) = input::read_fn(kb) {
                view.fn_held = held;
            }
        }

        // Touches.
        for event in touch.read() {
            if std::env::var_os("TOUCHBAR_DEBUG").is_some() {
                eprintln!("touch {event:?}");
            }
            last_touch = Instant::now();
            let px = |x: f64| x * display.width as f64;
            let hit = |x: f64| {
                extents
                    .iter()
                    .position(|&(a, b)| px(x) >= a - 4.0 && px(x) <= b + 4.0)
                    .filter(|&i| !slots[i].is_spacer())
            };
            match event {
                input::Touch::Down { finger, x } => {
                    if power != Power::On {
                        // The first touch only wakes the bar.
                        power = Power::On;
                        continue;
                    }
                    let Some(i) = hit(x) else { continue };
                    let keys = match &slots[i] {
                        Slot::Esc => vec![KeyCode::KEY_ESC],
                        Slot::Button(b) => b.keys(),
                        Slot::Back => Vec::new(),
                    };
                    keyboard.send(&keys, true);
                    presses[finger] = Press { slot: Some(i), keys };
                }
                input::Touch::Move { finger, x } => {
                    let press = &mut presses[finger];
                    if let Some(i) = press.slot {
                        let (a, b) = extents[i];
                        if px(x) < a - 20.0 || px(x) > b + 20.0 {
                            // Slid off the button: cancel.
                            keyboard.send(&press.keys, false);
                            *press = Press::default();
                        }
                    }
                }
                input::Touch::Up { finger } => {
                    let press = std::mem::take(&mut presses[finger]);
                    keyboard.send(&press.keys, false);
                    let Some(i) = press.slot else { continue };
                    match &slots[i] {
                        Slot::Back => {
                            view.opened.pop();
                        }
                        Slot::Button(b) => {
                            if let Some(target) = &b.open {
                                if view.opened.len() < 8 {
                                    view.opened.push(target.clone());
                                }
                            } else if let Some(target) = &b.switch {
                                view.base = target.clone();
                                view.opened.clear();
                            }
                        }
                        Slot::Esc => {}
                    }
                }
            }
        }

        // Config reload.
        let new_mtime = mtime(config_path);
        if new_mtime != config_mtime {
            config_mtime = new_mtime;
            match Config::load(config_path) {
                Ok(new) => {
                    eprintln!("config reloaded");
                    config = new;
                    icons = Icons::default();
                    view.repair(&config);
                }
                Err(e) => eprintln!("config not reloaded: {e:#}"),
            }
        }

        // Idle timers.
        let g = &config.general;
        let idle = last_touch.elapsed();
        let after = |secs: u64| secs > 0 && idle >= Duration::from_secs(secs);
        power = if after(g.off_after) {
            Power::Off
        } else if after(g.dim_after) {
            Power::Dim
        } else {
            Power::On
        };
        if after(g.collapse_after) {
            view.opened.clear();
        }

        // If the visible layer changed, any held key belongs to a button
        // that is gone: release everything.
        if view != before || view.slots(&config).len() != slots.len() {
            for press in presses.iter_mut() {
                keyboard.send(&press.keys, false);
                *press = Press::default();
            }
        }

        // Keyboards may come and go (e.g. after suspend).
        if fn_keyboards.is_empty() {
            fn_keyboards = input::fn_keyboards();
        }
    }

    for press in presses.iter_mut() {
        keyboard.send(&press.keys, false);
    }
    if let Some(bl) = backlight.as_mut() {
        bl.set(0);
    }
    eprintln!("stopped");
    Ok(())
}
