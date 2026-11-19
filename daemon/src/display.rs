//! The Touch Bar panel (a small DRM device) and its backlight.

use anyhow::{Context, Result, anyhow, bail};
use cairo::ImageSurface;
use drm::{
    Device as _,
    buffer::{Buffer as _, DrmFourcc},
    control::{ClipRect, Device as _, connector, dumbbuffer::DumbBuffer, framebuffer},
};
use std::{
    fs::{self, File, OpenOptions},
    os::fd::{AsFd, BorrowedFd},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl drm::control::Device for Card {}

pub struct Display {
    card: Card,
    buffer: DumbBuffer,
    framebuffer: framebuffer::Handle,
    /// Size of the landscape picture: the panel's long side by its short side.
    pub width: i32,
    pub height: i32,
    flip: bool,
    /// The last frame sent, in landscape pixels, to skip unchanged updates.
    last: Vec<u32>,
}

/// Finds the DRM card driven by `appletbdrm`.
fn find_card() -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let driver = fs::read_link(entry.path().join("device/driver")).ok();
        if driver.and_then(|d| d.file_name().map(|n| n == "appletbdrm")) == Some(true) {
            return Some(Path::new("/dev/dri").join(name));
        }
    }
    None
}

/// The Touch Bar starts in a basic keyboard mode; its second USB
/// configuration exposes the display. Switch to it if needed.
pub fn ensure_display_mode() {
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let read = |f: &str| fs::read_to_string(path.join(f)).map(|s| s.trim().to_string()).ok();
        if read("idVendor").as_deref() != Some("05ac") || read("idProduct").as_deref() != Some("8302") {
            continue;
        }
        if read("bConfigurationValue").as_deref() == Some("1") {
            eprintln!("switching the Touch Bar to display mode");
            let config = path.join("bConfigurationValue");
            if let Err(e) = fs::write(&config, "0")
                .and_then(|_| {
                    thread::sleep(Duration::from_millis(300));
                    fs::write(&config, "2")
                })
            {
                eprintln!("could not switch the Touch Bar mode: {e}");
            }
        }
    }
}

impl Display {
    /// Opens the panel, waiting up to `timeout` for it to appear.
    pub fn open(flip: bool, timeout: Duration) -> Result<Display> {
        let start = Instant::now();
        let path = loop {
            if let Some(path) = find_card() {
                break path;
            }
            if start.elapsed() > timeout {
                bail!("no Touch Bar display found (is the appletbdrm module loaded?)");
            }
            thread::sleep(Duration::from_millis(250));
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        let card = Card(file);
        card.acquire_master_lock().map_err(|e| {
            anyhow!("cannot take control of the Touch Bar ({e}); is tiny-dfr or another daemon running?")
        })?;

        let resources = card.resource_handles()?;
        let conn = resources
            .connectors()
            .iter()
            .filter_map(|&c| card.get_connector(c, true).ok())
            .find(|c| c.state() == connector::State::Connected)
            .ok_or_else(|| anyhow!("the Touch Bar display is not connected"))?;
        let mode = *conn.modes().first().ok_or_else(|| anyhow!("no display mode"))?;
        let crtc = *resources.crtcs().first().ok_or_else(|| anyhow!("no CRTC"))?;
        let (w, h) = mode.size();
        let (w, h) = (u32::from(w), u32::from(h));

        let mut buffer = card.create_dumb_buffer((w, h), DrmFourcc::Xrgb8888, 32)?;
        {
            let mut map = card.map_dumb_buffer(&mut buffer)?;
            map.as_mut().fill(0);
        }
        let framebuffer = card.add_framebuffer(&buffer, 24, 32)?;
        card.set_crtc(crtc, Some(framebuffer), (0, 0), &[conn.handle()], Some(mode))
            .context("setting the display mode")?;

        let (width, height) = (w.max(h) as i32, w.min(h) as i32);
        Ok(Display {
            card,
            buffer,
            framebuffer,
            width,
            height,
            flip,
            last: Vec::new(),
        })
    }

    /// Shows a landscape picture of `width` x `height`, rotated onto the panel.
    pub fn show(&mut self, surface: &mut ImageSurface) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        surface.flush();
        let stride = surface.stride() as usize;
        let pixels: Vec<u32> = {
            let data = surface.data()?;
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (y, x)))
                .map(|(y, x)| {
                    let at = y * stride + x * 4;
                    u32::from_ne_bytes(data[at..at + 4].try_into().unwrap()) & 0x00ff_ffff
                })
                .collect()
        };

        // Which landscape columns changed since the last frame?
        let changed: Vec<usize> = if self.last.len() == pixels.len() {
            (0..w)
                .filter(|&x| (0..h).any(|y| pixels[y * w + x] != self.last[y * w + x]))
                .collect()
        } else {
            (0..w).collect()
        };
        let (Some(&x0), Some(&x1)) = (changed.first(), changed.last()) else {
            return Ok(());
        };

        let pitch = self.buffer.pitch() as usize;
        let flip = self.flip;
        {
            let mut map = self.card.map_dumb_buffer(&mut self.buffer)?;
            let out = map.as_mut();
            // The panel is portrait: `h` pixels wide and `w` tall.
            for x in x0..=x1 {
                for y in 0..h {
                    let (px, py) = if flip { (y, w - 1 - x) } else { (h - 1 - y, x) };
                    let at = py * pitch + px * 4;
                    out[at..at + 4].copy_from_slice(&pixels[y * w + x].to_ne_bytes());
                }
            }
        }
        let (r0, r1) = if flip { (w - 1 - x1, w - 1 - x0) } else { (x0, x1) };
        let clip = ClipRect::new(0, r0 as u16, h as u16, r1 as u16 + 1);
        self.card
            .dirty_framebuffer(self.framebuffer, &[clip])
            .context("updating the display")?;
        self.last = pixels;
        Ok(())
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if let Ok(mut map) = self.card.map_dumb_buffer(&mut self.buffer) {
            map.as_mut().fill(0);
        }
        let full = ClipRect::new(0, 0, self.height as u16, self.width as u16);
        let _ = self.card.dirty_framebuffer(self.framebuffer, &[full]);
        let _ = self.card.release_master_lock();
    }
}

/// The Touch Bar backlight, with levels from 0 to `max`.
pub struct Backlight {
    path: PathBuf,
    pub max: u32,
    current: Option<u32>,
}

impl Backlight {
    pub fn open() -> Option<Backlight> {
        let path = Path::new("/sys/class/backlight/appletb_backlight").to_path_buf();
        let max = fs::read_to_string(path.join("max_brightness")).ok()?.trim().parse().ok()?;
        Some(Backlight { path, max, current: None })
    }

    pub fn set(&mut self, level: u32) {
        let level = level.min(self.max);
        if self.current == Some(level) {
            return;
        }
        match fs::write(self.path.join("brightness"), level.to_string()) {
            Ok(()) => self.current = Some(level),
            Err(e) => eprintln!("could not set the Touch Bar brightness: {e}"),
        }
    }
}
