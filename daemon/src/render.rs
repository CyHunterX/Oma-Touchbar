//! Drawing a row of buttons into a landscape image.

use crate::config::{Button, General};
use anyhow::Result;
use cairo::{Context, Format, ImageSurface};
use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

pub const HEIGHT: i32 = 60;
const MARGIN: f64 = 4.0;
const GAP: f64 = 8.0;
const BUTTON_TOP: f64 = 4.0;
const BUTTON_HEIGHT: f64 = 52.0;
const RADIUS: f64 = 8.0;

/// One visible position on the bar.
#[derive(Clone, Debug)]
pub enum Slot {
    /// The virtual Esc key.
    Esc,
    /// Closes the most recently opened layer.
    Back,
    Button(Button),
}

impl Slot {
    fn width(&self) -> u32 {
        match self {
            Slot::Button(b) => b.width(),
            _ => 1,
        }
    }

    pub fn is_spacer(&self) -> bool {
        matches!(self, Slot::Button(b) if b.is_spacer())
    }
}

/// Horizontal extent of each slot, in pixels.
pub fn layout(slots: &[Slot], width: i32) -> Vec<(f64, f64)> {
    let units: u32 = slots.iter().map(Slot::width).sum();
    if units == 0 {
        return Vec::new();
    }
    let usable = width as f64 - 2.0 * MARGIN - GAP * (units as f64 - 1.0);
    let unit = usable / units as f64;
    let mut x = MARGIN;
    slots
        .iter()
        .map(|slot| {
            let w = slot.width() as f64;
            let span = unit * w + GAP * (w - 1.0);
            let extent = (x, x + span);
            x += span + GAP;
            extent
        })
        .collect()
}

/// Expands the clock shorthands to strftime formats.
pub fn clock_format(spec: &str) -> &str {
    match spec {
        "24h" => "%H:%M",
        "12h" => "%-I:%M %p",
        other => other,
    }
}

/// Formats the current time; `None` if the format is invalid.
pub fn clock_text(spec: &str) -> Option<String> {
    let mut out = String::new();
    write!(out, "{}", chrono::Local::now().format(clock_format(spec))).ok()?;
    Some(out)
}

pub struct Battery {
    pub percent: u32,
    pub charging: bool,
}

pub fn battery() -> Option<Battery> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let read = |name: &str| fs::read_to_string(path.join(name)).ok();
        if read("type").as_deref().map(str::trim) != Some("Battery") {
            continue;
        }
        let percent = match read("capacity").and_then(|c| c.trim().parse().ok()) {
            Some(p) => p,
            None => {
                let num = |name: &str| read(name)?.trim().parse::<f64>().ok();
                let (now, full) = num("energy_now")
                    .zip(num("energy_full"))
                    .or_else(|| num("charge_now").zip(num("charge_full")))?;
                (now / full * 100.0).round() as u32
            }
        };
        let status = read("status").unwrap_or_default();
        return Some(Battery {
            percent: percent.min(100),
            charging: matches!(status.trim(), "Charging" | "Full"),
        });
    }
    None
}

/// Loads and caches icons, from files or the installed icon themes.
#[derive(Default)]
pub struct Icons {
    cache: HashMap<(String, u32), Option<ImageSurface>>,
    theme_index: Option<HashMap<String, PathBuf>>,
}

const ICON_DIRS: [&str; 2] = ["/etc/touchbar-daemon/icons", "/usr/share/touchbar-daemon/icons"];
const THEME_DIRS: [&str; 3] = [
    "/usr/share/icons/Adwaita",
    "/usr/share/icons/hicolor",
    "/usr/share/pixmaps",
];

impl Icons {
    pub fn get(&mut self, name: &str, size: u32) -> Option<ImageSurface> {
        let key = (name.to_string(), size);
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let surface = self.find(name).and_then(|path| match load_icon(&path, size) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("icon {}: {e}", path.display());
                None
            }
        });
        if surface.is_none() {
            eprintln!("icon \"{name}\" not found");
        }
        self.cache.insert(key, surface.clone());
        surface
    }

    /// Names of the icons the daemon can find by name, sorted, with
    /// monochrome "-symbolic" icons first since they suit the bar best.
    pub fn names(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self.theme_index.get_or_insert_with(index_themes).keys().cloned().collect();
        for dir in ICON_DIRS {
            if let Ok(entries) = fs::read_dir(dir) {
                names.extend(entries.flatten().filter_map(|e| {
                    let p = e.path();
                    matches!(p.extension()?.to_str()?, "svg" | "png")
                        .then(|| p.file_stem()?.to_str().map(String::from))?
                }));
            }
        }
        names.sort_by(|a, b| {
            (!a.ends_with("-symbolic"), a).cmp(&(!b.ends_with("-symbolic"), b))
        });
        names.dedup();
        names
    }

    fn find(&mut self, name: &str) -> Option<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return path.exists().then(|| path.to_path_buf());
        }
        for dir in ICON_DIRS {
            for ext in ["svg", "png"] {
                let p = Path::new(dir).join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        self.theme_index
            .get_or_insert_with(index_themes)
            .get(name)
            .cloned()
    }
}

/// Maps icon names to the best file for each across the theme directories.
fn index_themes() -> HashMap<String, PathBuf> {
    fn score(path: &Path) -> u32 {
        let s = path.to_string_lossy();
        match path.extension().and_then(|e| e.to_str()) {
            Some("svg") if s.contains("symbolic") || s.contains("scalable") => 3,
            Some("svg") => 2,
            Some("png") if s.contains("256") || s.contains("512") => 1,
            _ => 0,
        }
    }
    fn walk(dir: &Path, index: &mut HashMap<String, PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, index);
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            if !matches!(ext, Some("svg" | "png")) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let better = index.get(stem).is_none_or(|old| score(&path) > score(old));
            if better {
                index.insert(stem.to_string(), path.clone());
            }
        }
    }
    let mut index = HashMap::new();
    // Earlier directories win ties, so walk them last.
    for dir in THEME_DIRS.iter().rev() {
        let mut theme = HashMap::new();
        walk(Path::new(dir), &mut theme);
        index.extend(theme);
    }
    index
}

/// Renders an icon file into a square surface of `size` pixels.
fn load_icon(path: &Path, size: u32) -> Result<ImageSurface> {
    let is_png = path.extension().and_then(|e| e.to_str()) == Some("png");
    if is_png {
        let mut file = fs::File::open(path)?;
        let png = ImageSurface::create_from_png(&mut file)?;
        let out = ImageSurface::create(Format::ARgb32, size as i32, size as i32)?;
        let cr = Context::new(&out)?;
        let scale = size as f64 / png.width().max(png.height()) as f64;
        cr.scale(scale, scale);
        cr.set_source_surface(&png, 0.0, 0.0)?;
        cr.paint()?;
        drop(cr);
        return Ok(out);
    }

    use resvg::{tiny_skia, usvg};
    let data = fs::read(path)?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or_else(|| anyhow::anyhow!("bad size"))?;
    let tree_size = tree.size();
    let scale = size as f32 / tree_size.width().max(tree_size.height());
    let dx = (size as f32 - tree_size.width() * scale) / 2.0;
    let dy = (size as f32 - tree_size.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia is premultiplied RGBA; cairo is premultiplied native-endian ARGB.
    let mut out = ImageSurface::create(Format::ARgb32, size as i32, size as i32)?;
    let stride = out.stride() as usize;
    {
        let mut dst = out.data()?;
        for (y, row) in pixmap.data().chunks_exact(size as usize * 4).enumerate() {
            for (x, px) in row.chunks_exact(4).enumerate() {
                let argb = u32::from(px[3]) << 24
                    | u32::from(px[0]) << 16
                    | u32::from(px[1]) << 8
                    | u32::from(px[2]);
                let at = y * stride + x * 4;
                dst[at..at + 4].copy_from_slice(&argb.to_ne_bytes());
            }
        }
    }
    Ok(out)
}

/// Draws the bar. `pressed` is the index of the slot being touched.
pub fn render(
    general: &General,
    slots: &[Slot],
    pressed: Option<usize>,
    width: i32,
    icons: &mut Icons,
) -> Result<ImageSurface> {
    let surface = ImageSurface::create(Format::Rgb24, width, HEIGHT)?;
    let cr = Context::new(&surface)?;
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.paint()?;

    let font = pango::FontDescription::from_string(&format!(
        "{} {}px",
        general.font, general.font_size
    ));
    let icon_size = general.icon_size.round() as u32;

    for (i, (slot, (x0, x1))) in slots.iter().zip(layout(slots, width)).enumerate() {
        if slot.is_spacer() {
            continue;
        }
        let is_pressed = pressed == Some(i);
        if general.outlines || is_pressed {
            let shade = if is_pressed { 0.36 } else { 0.17 };
            rounded_rect(&cr, x0, BUTTON_TOP, x1 - x0, BUTTON_HEIGHT, RADIUS);
            cr.set_source_rgb(shade, shade, shade);
            cr.fill()?;
        }

        // Build the content as a list of pieces laid out left to right.
        let mut pieces: Vec<Piece> = Vec::new();
        let mut color = (1.0, 1.0, 1.0);
        match slot {
            Slot::Esc => pieces.push(Piece::Text("esc".into())),
            Slot::Back => pieces.push(Piece::Chevron),
            Slot::Button(b) => {
                if let Some(spec) = &b.clock {
                    pieces.push(Piece::Text(clock_text(spec).unwrap_or_else(|| "bad format".into())));
                } else if let Some(style) = &b.battery {
                    match battery() {
                        Some(bat) => {
                            if bat.charging {
                                color = (0.35, 0.85, 0.4);
                            } else if bat.percent < 10 {
                                color = (0.95, 0.3, 0.3);
                            }
                            if style != "percent" {
                                pieces.push(Piece::Battery(bat.percent, bat.charging));
                            }
                            if style != "icon" {
                                pieces.push(Piece::Text(format!("{}%", bat.percent)));
                            }
                        }
                        None => pieces.push(Piece::Text("no battery".into())),
                    }
                } else {
                    if let Some(name) = &b.icon {
                        match icons.get(name, icon_size) {
                            Some(surface) => pieces.push(Piece::Icon(surface, b.tint.unwrap_or(true))),
                            None => pieces.push(Piece::Text("?".into())),
                        }
                    }
                    if let Some(label) = &b.label {
                        pieces.push(Piece::Text(label.clone()));
                    }
                }
            }
        }

        // Measure, then draw centred.
        let spacing = 6.0;
        let available = (x1 - x0 - 12.0).max(1.0);
        let layouts: Vec<Option<pango::Layout>> = pieces
            .iter()
            .map(|p| match p {
                Piece::Text(text) => {
                    let layout = pangocairo::functions::create_layout(&cr);
                    layout.set_font_description(Some(&font));
                    layout.set_text(text);
                    layout.set_ellipsize(pango::EllipsizeMode::End);
                    layout.set_width((available * pango::SCALE as f64) as i32);
                    Some(layout)
                }
                _ => None,
            })
            .collect();
        let widths: Vec<f64> = pieces
            .iter()
            .zip(&layouts)
            .map(|(p, l)| match (p, l) {
                (_, Some(layout)) => layout.pixel_size().0 as f64,
                (Piece::Icon(s, _), _) => s.width() as f64,
                (Piece::Battery(..), _) => 40.0,
                (Piece::Chevron, _) => 20.0,
                _ => 0.0,
            })
            .collect();
        let total: f64 = widths.iter().sum::<f64>() + spacing * (pieces.len().max(1) - 1) as f64;
        let mut x = x0 + ((x1 - x0) - total).max(0.0) / 2.0;
        let mid = BUTTON_TOP + BUTTON_HEIGHT / 2.0;
        cr.save()?;
        rounded_rect(&cr, x0, BUTTON_TOP, x1 - x0, BUTTON_HEIGHT, RADIUS);
        cr.clip();
        for ((piece, layout), w) in pieces.iter().zip(&layouts).zip(&widths) {
            cr.set_source_rgb(color.0, color.1, color.2);
            match piece {
                Piece::Text(_) => {
                    let layout = layout.as_ref().unwrap();
                    let h = layout.pixel_size().1 as f64;
                    cr.move_to(x, mid - h / 2.0);
                    pangocairo::functions::show_layout(&cr, layout);
                }
                Piece::Icon(surface, tint) => {
                    let y = mid - surface.height() as f64 / 2.0;
                    if *tint {
                        cr.mask_surface(surface, x, y)?;
                    } else {
                        cr.set_source_surface(surface, x, y)?;
                        cr.paint()?;
                    }
                }
                Piece::Battery(percent, charging) => draw_battery(&cr, x, mid, *percent, *charging)?,
                Piece::Chevron => {
                    cr.set_line_width(3.5);
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_join(cairo::LineJoin::Round);
                    cr.move_to(x + 14.0, mid - 11.0);
                    cr.line_to(x + 4.0, mid);
                    cr.line_to(x + 14.0, mid + 11.0);
                    cr.stroke()?;
                }
            }
            x += w + spacing;
        }
        cr.restore()?;
    }
    drop(cr);
    Ok(surface)
}

enum Piece {
    Text(String),
    Icon(ImageSurface, bool),
    Battery(u32, bool),
    Chevron,
}

fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn draw_battery(cr: &Context, x: f64, mid: f64, percent: u32, charging: bool) -> Result<()> {
    let (w, h) = (34.0, 18.0);
    let top = mid - h / 2.0;
    cr.set_line_width(2.0);
    rounded_rect(cr, x + 1.0, top, w, h, 4.0);
    cr.stroke()?;
    cr.rectangle(x + w + 2.0, mid - 4.0, 3.0, 8.0);
    cr.fill()?;
    let level = (w - 6.0) * percent.min(100) as f64 / 100.0;
    if level > 0.0 {
        rounded_rect(cr, x + 4.0, top + 3.0, level.max(3.0), h - 6.0, 1.5);
        cr.fill()?;
    }
    if charging {
        // A small bolt over the body, cut out in black.
        cr.save()?;
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.move_to(x + 20.0, top + 1.0);
        cr.line_to(x + 12.0, mid + 1.5);
        cr.line_to(x + 18.0, mid + 1.5);
        cr.line_to(x + 15.0, top + h - 1.0);
        cr.line_to(x + 24.0, mid - 1.5);
        cr.line_to(x + 18.0, mid - 1.5);
        cr.close_path();
        cr.fill()?;
        cr.restore()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fills_the_bar() {
        let b = |w| Slot::Button(Button { label: Some("x".into()), width: Some(w), ..Default::default() });
        let slots = [b(1), b(2), b(1)];
        let l = layout(&slots, 2008);
        assert_eq!(l.len(), 3);
        assert!((l[0].0 - MARGIN).abs() < 1e-9);
        assert!((l[2].1 - (2008.0 - MARGIN)).abs() < 1e-6);
        // A width-2 button covers two units plus the gap between them.
        let unit = l[0].1 - l[0].0;
        assert!(((l[1].1 - l[1].0) - (2.0 * unit + GAP)).abs() < 1e-6);
    }

    #[test]
    fn clock_formats() {
        assert!(clock_text("24h").is_some());
        assert!(clock_text("%H:%M %a").is_some());
    }
}
