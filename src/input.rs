//! Touch input from the bar, the Fn key, and the virtual keyboard that
//! sends the buttons' keys.

use anyhow::{Context, Result, anyhow, bail};
use evdev::{
    AbsoluteAxisCode, AttributeSet, Device, EventSummary, EventType, InputEvent, KeyCode,
    uinput::VirtualDevice,
};
use std::{
    thread,
    time::{Duration, Instant},
};

/// A finger on the bar changed state.
#[derive(Debug, PartialEq)]
pub enum Touch {
    Down { finger: usize, x: f64 },
    Move { finger: usize, x: f64 },
    Up { finger: usize },
}

#[derive(Clone, Copy, Default)]
struct Finger {
    active: bool,
    was_active: bool,
    x: f64,
    moved: bool,
}

pub struct TouchBar {
    pub device: Device,
    min: f64,
    max: f64,
    flip: bool,
    slot: usize,
    fingers: [Finger; 10],
}

impl TouchBar {
    /// Opens and grabs the Touch Bar's touch surface, waiting up to `timeout`.
    pub fn open(flip: bool, timeout: Duration) -> Result<TouchBar> {
        let start = Instant::now();
        let mut device = loop {
            let found = evdev::enumerate().map(|(_, d)| d).find(|d| {
                let name = d.name().unwrap_or_default();
                name.contains("Touch Bar")
                    && d.supported_absolute_axes()
                        .is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_MT_POSITION_X))
            });
            if let Some(device) = found {
                break device;
            }
            if start.elapsed() > timeout {
                bail!("no Touch Bar touch input found");
            }
            thread::sleep(Duration::from_millis(250));
        };
        let info = device
            .get_absinfo()?
            .find(|(axis, _)| *axis == AbsoluteAxisCode::ABS_MT_POSITION_X)
            .map(|(_, info)| info)
            .ok_or_else(|| anyhow!("touch input has no X axis"))?;
        // Keep touches away from the compositor's pointer.
        device.grab().context("grabbing the touch input")?;
        device.set_nonblocking(true)?;
        Ok(TouchBar {
            device,
            min: info.minimum() as f64,
            max: info.maximum().max(info.minimum() + 1) as f64,
            flip,
            slot: 0,
            fingers: [Finger::default(); 10],
        })
    }

    /// Reads pending events; positions are fractions of the bar's length (0–1).
    pub fn read(&mut self) -> Vec<Touch> {
        let mut out = Vec::new();
        let events: Vec<InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return out,
            Err(e) => {
                eprintln!("touch input: {e}");
                return out;
            }
        };
        for event in events {
            match event.destructure() {
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_SLOT, v) => {
                    self.slot = (v.max(0) as usize).min(self.fingers.len() - 1);
                }
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_TRACKING_ID, v) => {
                    self.fingers[self.slot].active = v >= 0;
                }
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_X, v) => {
                    let mut x = ((v as f64 - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
                    if self.flip {
                        x = 1.0 - x;
                    }
                    let f = &mut self.fingers[self.slot];
                    f.x = x;
                    f.moved = true;
                }
                EventSummary::Synchronization(..) => {
                    for (i, f) in self.fingers.iter_mut().enumerate() {
                        match (f.was_active, f.active) {
                            (false, true) => out.push(Touch::Down { finger: i, x: f.x }),
                            (true, false) => out.push(Touch::Up { finger: i }),
                            (true, true) if f.moved => out.push(Touch::Move { finger: i, x: f.x }),
                            _ => {}
                        }
                        f.was_active = f.active;
                        f.moved = false;
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Keyboards that report the Fn key.
pub fn fn_keyboards() -> Vec<Device> {
    evdev::enumerate()
        .map(|(_, d)| d)
        .filter(|d| {
            !d.name().unwrap_or_default().contains("Touch Bar")
                && d.supported_keys().is_some_and(|k| k.contains(KeyCode::KEY_FN))
        })
        .filter(|d| d.set_nonblocking(true).is_ok())
        .collect()
}

/// Returns the new Fn state if this keyboard reported a change.
pub fn read_fn(device: &mut Device) -> Option<bool> {
    let mut state = None;
    if let Ok(events) = device.fetch_events() {
        for event in events {
            if let EventSummary::Key(_, KeyCode::KEY_FN, value) = event.destructure() {
                match value {
                    1 => state = Some(true),
                    0 => state = Some(false),
                    _ => {}
                }
            }
        }
    }
    state
}

pub struct Keyboard(VirtualDevice);

impl Keyboard {
    pub fn new() -> Result<Keyboard> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in (1..0x100u16).chain(0x160..0x2ff) {
            keys.insert(KeyCode::new(code));
        }
        let device = VirtualDevice::builder()
            .context("opening /dev/uinput")?
            .name("touchbar-daemon virtual keyboard")
            .with_keys(&keys)?
            .build()?;
        Ok(Keyboard(device))
    }

    /// Presses (`down = true`) or releases a combo; releases in reverse order.
    pub fn send(&mut self, keys: &[KeyCode], down: bool) {
        let value = i32::from(down);
        let events: Vec<InputEvent> = if down {
            keys.iter().map(|k| InputEvent::new(EventType::KEY.0, k.code(), value)).collect()
        } else {
            keys.iter().rev().map(|k| InputEvent::new(EventType::KEY.0, k.code(), value)).collect()
        };
        if let Err(e) = self.0.emit(&events) {
            eprintln!("sending keys: {e}");
        }
    }
}
