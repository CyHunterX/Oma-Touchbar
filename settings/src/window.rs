//! The settings window.

use crate::store::{self, SaveError};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use touchbar_daemon::{
    config::{self, Button, Config, Layer},
    render::{self, Icons, Slot},
};

const BAR_WIDTH: i32 = 2008;

/// What kind of button the editor is making.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Key,
    Group,
    Switch,
    Clock,
    Battery,
    Spacer,
}

const KINDS: [(Kind, &str); 6] = [
    (Kind::Key, "Key button"),
    (Kind::Group, "Group (expands more buttons)"),
    (Kind::Switch, "Switch to another layer"),
    (Kind::Clock, "Clock"),
    (Kind::Battery, "Battery"),
    (Kind::Spacer, "Empty space"),
];

fn kind_of(b: &Button) -> Kind {
    if b.open.is_some() {
        Kind::Group
    } else if b.switch.is_some() {
        Kind::Switch
    } else if b.clock.is_some() {
        Kind::Clock
    } else if b.battery.is_some() {
        Kind::Battery
    } else if b.is_spacer() {
        Kind::Spacer
    } else {
        Kind::Key
    }
}

struct Ui {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    preview: gtk::DrawingArea,
    crumbs: gtk::Box,
    info: gtk::Label,
    list: gtk::ListBox,
    layers: gtk::ListBox,
    undo: gtk::Button,
    general: General,
    path: PathBuf,
    config: RefCell<Config>,
    history: RefCell<Vec<Config>>,
    /// The layer being edited, and the groups opened from it.
    nav: RefCell<Vec<String>>,
    icons: RefCell<Icons>,
    icon_names: RefCell<Option<Vec<String>>>,
    keys: Vec<String>,
    /// Bumped on every change, so only the last of a burst gets saved.
    save_generation: Cell<u64>,
    /// False while the file on disk could not be read, so it is not
    /// overwritten until the user chooses to start over.
    can_save: Cell<bool>,
    /// Set while widgets are being filled in from the config.
    loading: Cell<bool>,
}

struct General {
    default_layer: adw::ComboRow,
    fn_layer: adw::ComboRow,
    outlines: adw::SwitchRow,
    font: adw::EntryRow,
    font_size: adw::SpinRow,
    icon_size: adw::SpinRow,
    dim_after: adw::SpinRow,
    off_after: adw::SpinRow,
    collapse_after: adw::SpinRow,
    esc_button: adw::SwitchRow,
    flip_touch: adw::SwitchRow,
    flip_display: adw::SwitchRow,
}

pub fn build(app: &adw::Application) {
    let path = store::path();
    let (config, load_error) = store::load(&path);

    // Header
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    let menu = gio::Menu::new();
    menu.append(Some("Reset to Defaults"), Some("win.reset"));
    menu.append(Some("About Oma Touchbar"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&undo);
    header.pack_end(&menu_button);

    let banner = adw::Banner::new("");

    // Preview
    let preview = gtk::DrawingArea::builder()
        .content_height(56)
        .hexpand(true)
        .build();
    preview.set_cursor_from_name(Some("pointer"));
    let preview_frame = gtk::Frame::new(None);
    preview_frame.set_child(Some(&preview));
    let hint = gtk::Label::new(Some("Click a button in the preview to edit it, or a group to open it"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");

    // Navigation
    let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    crumbs.add_css_class("linked");
    let info = gtk::Label::new(None);
    info.add_css_class("dim-label");
    info.set_xalign(0.0);
    info.set_wrap(true);

    // Buttons of the current layer
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let add_menu = gio::Menu::new();
    for (kind, label) in KINDS {
        add_menu.append(Some(label), Some(&format!("win.add('{kind:?}')")));
    }
    let add = gtk::MenuButton::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("Add Button")
                .build(),
        )
        .menu_model(&add_menu)
        .halign(gtk::Align::Center)
        .build();
    add.add_css_class("pill");
    let buttons_group = adw::PreferencesGroup::new();
    buttons_group.add(&list);

    // Layers
    let layers = gtk::ListBox::new();
    layers.add_css_class("boxed-list");
    layers.set_selection_mode(gtk::SelectionMode::None);
    let new_layer = gtk::Button::from_icon_name("list-add-symbolic");
    new_layer.add_css_class("flat");
    new_layer.set_tooltip_text(Some("New layer"));
    let layers_group = adw::PreferencesGroup::builder()
        .title("All Layers")
        .description("Groups are layers too: a group button opens its layer over the bar")
        .header_suffix(&new_layer)
        .build();
    layers_group.add(&layers);

    // General
    let spin = |title: &str, subtitle: &str, min: f64, max: f64| {
        let row = adw::SpinRow::with_range(min, max, 1.0);
        row.set_title(title);
        row.set_subtitle(subtitle);
        row
    };
    let switch = |title: &str, subtitle: &str| {
        adw::SwitchRow::builder().title(title).subtitle(subtitle).build()
    };
    let general = General {
        default_layer: adw::ComboRow::builder().title("Default layer").build(),
        fn_layer: adw::ComboRow::builder()
            .title("Fn layer")
            .subtitle("Shown while holding Fn")
            .build(),
        outlines: switch("Button backgrounds", "Draw a shape behind each button"),
        font: adw::EntryRow::builder().title("Font (e.g. Sans Bold)").build(),
        font_size: spin("Text size", "Pixels", 4.0, 60.0),
        icon_size: spin("Icon size", "Pixels", 4.0, 60.0),
        dim_after: spin("Dim after", "Seconds without a touch; 0 never dims", 0.0, 3600.0),
        off_after: spin("Turn off after", "Seconds without a touch; 0 stays on", 0.0, 3600.0),
        collapse_after: spin("Close groups after", "Seconds without a touch; 0 keeps them open", 0.0, 3600.0),
        esc_button: switch("Esc button", "For models without a physical Esc key"),
        flip_touch: switch("Mirror touches", "If presses land on the opposite button"),
        flip_display: switch("Rotate picture", "If the bar shows upside down"),
    };
    let general_group = adw::PreferencesGroup::builder().title("General").build();
    for row in [
        general.default_layer.upcast_ref::<gtk::Widget>(),
        general.fn_layer.upcast_ref(),
        general.outlines.upcast_ref(),
        general.font.upcast_ref(),
        general.font_size.upcast_ref(),
        general.icon_size.upcast_ref(),
        general.dim_after.upcast_ref(),
        general.off_after.upcast_ref(),
        general.collapse_after.upcast_ref(),
        general.esc_button.upcast_ref(),
        general.flip_touch.upcast_ref(),
        general.flip_display.upcast_ref(),
    ] {
        general_group.add(row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&preview_frame);
    content.append(&hint);
    content.append(&crumbs);
    content.append(&info);
    content.append(&buttons_group);
    content.append(&add);
    content.append(&layers_group);
    content.append(&general_group);
    let clamp = adw::Clamp::builder().maximum_size(1100).child(&content).build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner);
    body.append(&scrolled);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Oma Touchbar")
        .default_width(1180)
        .default_height(860)
        .content(&view)
        .build();

    let root = config.general.default_layer.clone();
    let ui = Rc::new(Ui {
        window,
        toasts,
        banner,
        preview,
        crumbs,
        info,
        list,
        layers,
        undo,
        general,
        path,
        config: RefCell::new(config),
        history: RefCell::new(Vec::new()),
        nav: RefCell::new(vec![root]),
        icons: RefCell::new(Icons::default()),
        icon_names: RefCell::new(None),
        keys: config::key_names(),
        save_generation: Cell::new(0),
        can_save: Cell::new(load_error.is_none()),
        loading: Cell::new(false),
    });

    ui.connect_signals(&new_layer);
    ui.fill_general();
    ui.refresh();
    if let Some(e) = load_error {
        ui.show_banner(
            &format!("Your config can't be read, so changes won't be saved: {e}"),
            "Start From Defaults",
        );
    }
    ui.window.present();
}

impl Ui {
    fn connect_signals(self: &Rc<Self>, new_layer: &gtk::Button) {
        // Preview: draw with the daemon's own renderer.
        {
            let ui = Rc::downgrade(self);
            self.preview.set_draw_func(move |_, cr, w, h| {
                let Some(ui) = ui.upgrade() else { return };
                ui.draw_preview(cr, w, h);
            });
        }
        let click = gtk::GestureClick::new();
        {
            let ui = self.clone();
            click.connect_released(move |_, _, x, _| ui.preview_clicked(x));
        }
        self.preview.add_controller(click);

        {
            let ui = self.clone();
            self.undo.connect_clicked(move |_| ui.undo());
        }
        {
            let ui = self.clone();
            new_layer.connect_clicked(move |_| ui.new_layer());
        }
        {
            let ui = self.clone();
            self.banner.connect_button_clicked(move |_| ui.banner_action());
        }

        // Window actions
        let add = gio::SimpleAction::new("add", Some(glib::VariantTy::STRING));
        {
            let ui = self.clone();
            add.connect_activate(move |_, arg| {
                let name = arg.and_then(|a| a.get::<String>()).unwrap_or_default();
                let kind = KINDS.iter().map(|(k, _)| *k).find(|k| format!("{k:?}") == name);
                ui.edit_dialog(None, kind.unwrap_or(Kind::Key));
            });
        }
        self.window.add_action(&add);
        let reset = gio::SimpleAction::new("reset", None);
        {
            let ui = self.clone();
            reset.connect_activate(move |_, _| {
                let defaults = Config::parse(config::DEFAULT_CONFIG).unwrap();
                ui.change(|c| *c = defaults);
                ui.toast("Defaults restored — Undo brings your layout back");
            });
        }
        self.window.add_action(&reset);
        let about = gio::SimpleAction::new("about", None);
        {
            let ui = self.clone();
            about.connect_activate(move |_, _| {
                adw::AboutDialog::builder()
                    .application_name("Oma Touchbar")
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("Settings for the Oma Touchbar daemon")
                    .website("https://github.com/CyHunterX/Oma-Touchbar")
                    .license_type(gtk::License::MitX11)
                    .build()
                    .present(Some(&ui.window));
            });
        }
        self.window.add_action(&about);
        let undo = gio::SimpleAction::new("undo", None);
        {
            let ui = self.clone();
            undo.connect_activate(move |_, _| ui.undo());
        }
        self.window.add_action(&undo);
        if let Some(app) = self.window.application() {
            app.set_accels_for_action("win.undo", &["<Control>z"]);
        }

        // General options
        let g = &self.general;
        for row in [&g.default_layer, &g.fn_layer] {
            let ui = self.clone();
            row.connect_selected_notify(move |_| ui.read_general());
        }
        for row in [&g.outlines, &g.esc_button, &g.flip_touch, &g.flip_display] {
            let ui = self.clone();
            row.connect_active_notify(move |_| ui.read_general());
        }
        for row in [&g.font_size, &g.icon_size, &g.dim_after, &g.off_after, &g.collapse_after] {
            let ui = self.clone();
            row.connect_value_notify(move |_| ui.read_general());
        }
        {
            let ui = self.clone();
            g.font.connect_changed(move |_| ui.read_general());
        }
    }

    fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    fn show_banner(&self, title: &str, button: &str) {
        self.banner.set_title(title);
        self.banner.set_button_label(Some(button));
        self.banner.set_revealed(true);
    }

    fn banner_action(self: &Rc<Self>) {
        self.banner.set_revealed(false);
        if !self.can_save.get() {
            // "Start From Defaults": the unreadable file may now be replaced.
            self.can_save.set(true);
            self.schedule_save();
            return;
        }
        // "Allow": grant write access once, then save.
        match store::grant_permission(&self.path) {
            Ok(true) => self.save_now(),
            Ok(false) => self.show_banner("Not allowed — changes are not saved yet", "Try Again"),
            Err(e) => self.toast(&format!("Could not ask for permission: {e}")),
        }
    }

    // ----- state -----

    fn current(&self) -> String {
        self.nav.borrow().last().cloned().unwrap_or_default()
    }

    fn current_buttons(&self) -> Vec<Button> {
        let name = self.current();
        self.config.borrow().layer(&name).map(|l| l.buttons.clone()).unwrap_or_default()
    }

    /// Applies an edit: records it for undo, refreshes, and saves.
    fn change(self: &Rc<Self>, edit: impl FnOnce(&mut Config)) {
        let before = self.config.borrow().clone();
        {
            let mut config = self.config.borrow_mut();
            edit(&mut config);
            if *config == before {
                return;
            }
        }
        let mut history = self.history.borrow_mut();
        history.push(before);
        if history.len() > 200 {
            history.remove(0);
        }
        drop(history);
        self.after_change();
    }

    fn edit_current(self: &Rc<Self>, edit: impl FnOnce(&mut Vec<Button>)) {
        let name = self.current();
        self.change(|c| {
            if let Some(layer) = c.layers.iter_mut().find(|l| l.name == name) {
                edit(&mut layer.buttons);
            }
        });
    }

    fn undo(self: &Rc<Self>) {
        let Some(previous) = self.history.borrow_mut().pop() else { return };
        *self.config.borrow_mut() = previous;
        self.after_change();
    }

    fn after_change(self: &Rc<Self>) {
        // Keep the navigation pointing at layers that still exist.
        {
            let config = self.config.borrow();
            let mut nav = self.nav.borrow_mut();
            nav.retain(|l| config.layer(l).is_some());
            if nav.is_empty() {
                nav.push(config.general.default_layer.clone());
            }
        }
        self.fill_general();
        self.refresh();
        self.schedule_save();
    }

    fn schedule_save(self: &Rc<Self>) {
        let generation = self.save_generation.get() + 1;
        self.save_generation.set(generation);
        let ui = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            if ui.save_generation.get() == generation {
                ui.save_now();
            }
        });
    }

    fn save_now(self: &Rc<Self>) {
        if !self.can_save.get() {
            return;
        }
        let config = self.config.borrow().clone();
        if let Err(e) = config.check() {
            self.show_banner(&format!("Not saved: {e:#}"), "OK");
            return;
        }
        match store::save(&self.path, &config) {
            Ok(()) => {
                if self.banner.is_revealed() {
                    self.banner.set_revealed(false);
                }
            }
            Err(SaveError::NeedsPermission) => self.show_banner(
                "Changes aren't saved yet: this app needs permission to edit the Touch Bar config (asked once)",
                "Allow",
            ),
            Err(SaveError::Other(e)) => self.show_banner(&format!("Not saved: {e}"), "Retry"),
        }
    }

    // ----- general options -----

    fn fill_general(&self) {
        self.loading.set(true);
        let config = self.config.borrow();
        let g = &config.general;
        let names: Vec<&str> = config.layers.iter().map(|l| l.name.as_str()).collect();
        let w = &self.general;
        w.default_layer.set_model(Some(&gtk::StringList::new(&names)));
        w.default_layer
            .set_selected(names.iter().position(|n| *n == g.default_layer).unwrap_or(0) as u32);
        let mut fn_names = vec!["None"];
        fn_names.extend(&names);
        w.fn_layer.set_model(Some(&gtk::StringList::new(&fn_names)));
        w.fn_layer.set_selected(
            names.iter().position(|n| *n == g.fn_layer).map(|p| p + 1).unwrap_or(0) as u32,
        );
        w.outlines.set_active(g.outlines);
        if w.font.text() != g.font {
            w.font.set_text(&g.font);
        }
        w.font_size.set_value(g.font_size);
        w.icon_size.set_value(g.icon_size);
        w.dim_after.set_value(g.dim_after as f64);
        w.off_after.set_value(g.off_after as f64);
        w.collapse_after.set_value(g.collapse_after as f64);
        w.esc_button.set_active(g.esc_button);
        w.flip_touch.set_active(g.flip_touch);
        w.flip_display.set_active(g.flip_display);
        drop(config);
        self.loading.set(false);
    }

    fn read_general(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        let w = &self.general;
        let names: Vec<String> = self.config.borrow().layers.iter().map(|l| l.name.clone()).collect();
        let default_layer = names.get(w.default_layer.selected() as usize).cloned();
        let fn_layer = match w.fn_layer.selected() {
            0 => String::new(),
            i => names.get(i as usize - 1).cloned().unwrap_or_default(),
        };
        let font = w.font.text().to_string();
        let values = (
            w.outlines.is_active(),
            w.font_size.value(),
            w.icon_size.value(),
            w.dim_after.value() as u64,
            w.off_after.value() as u64,
            w.collapse_after.value() as u64,
            w.esc_button.is_active(),
            w.flip_touch.is_active(),
            w.flip_display.is_active(),
        );
        self.change(|c| {
            let g = &mut c.general;
            if let Some(d) = default_layer {
                g.default_layer = d;
            }
            g.fn_layer = fn_layer;
            g.font = font;
            (
                g.outlines,
                g.font_size,
                g.icon_size,
                g.dim_after,
                g.off_after,
                g.collapse_after,
                g.esc_button,
                g.flip_touch,
                g.flip_display,
            ) = values;
        });
    }

    // ----- views -----

    /// The slots the bar would show for the layer being edited.
    fn slots(&self) -> Vec<Slot> {
        let config = self.config.borrow();
        let mut slots = Vec::new();
        if config.general.esc_button {
            slots.push(Slot::Esc);
        }
        if self.nav.borrow().len() > 1 {
            slots.push(Slot::Back);
        }
        slots.extend(self.current_buttons().into_iter().map(Slot::Button));
        slots
    }

    fn draw_preview(&self, cr: &gtk::cairo::Context, w: i32, h: i32) {
        let slots = self.slots();
        let general = self.config.borrow().general.clone();
        let surface = match render::render(&general, &slots, None, BAR_WIDTH, &mut self.icons.borrow_mut()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let scale = (w as f64 / BAR_WIDTH as f64).min(h as f64 / render::HEIGHT as f64);
        let dy = (h as f64 - render::HEIGHT as f64 * scale) / 2.0;
        cr.set_source_rgb(0.0, 0.0, 0.0);
        let _ = cr.paint();
        cr.translate(0.0, dy);
        cr.scale(scale, scale);
        let _ = cr.set_source_surface(&surface, 0.0, 0.0);
        cr.source().set_filter(gtk::cairo::Filter::Best);
        let _ = cr.paint();
    }

    fn preview_clicked(self: &Rc<Self>, x: f64) {
        let width = self.preview.width() as f64;
        let height = self.preview.height() as f64;
        let scale = (width / BAR_WIDTH as f64).min(height / render::HEIGHT as f64);
        let bar_x = x / scale;
        let slots = self.slots();
        let Some(i) = render::layout(&slots, BAR_WIDTH)
            .iter()
            .position(|&(a, b)| bar_x >= a && bar_x <= b)
        else {
            return;
        };
        let offset = slots.iter().filter(|s| matches!(s, Slot::Esc | Slot::Back)).count();
        match &slots[i] {
            Slot::Back => self.go_back(),
            Slot::Esc => self.toast("The Esc button is added by the “Esc button” option below"),
            Slot::Button(b) => match &b.open {
                Some(target) => self.open_group(target),
                None => self.edit_dialog(Some(i - offset), kind_of(b)),
            },
        }
    }

    fn open_group(self: &Rc<Self>, layer: &str) {
        if self.nav.borrow().len() < 8 {
            self.nav.borrow_mut().push(layer.to_string());
            self.refresh();
        }
    }

    fn go_back(self: &Rc<Self>) {
        if self.nav.borrow().len() > 1 {
            self.nav.borrow_mut().pop();
            self.refresh();
        }
    }

    fn set_root(self: &Rc<Self>, layer: &str) {
        *self.nav.borrow_mut() = vec![layer.to_string()];
        self.refresh();
    }

    fn role(&self, name: &str) -> String {
        let config = self.config.borrow();
        let mut roles = Vec::new();
        if config.general.default_layer == name {
            roles.push("shown by default".to_string());
        }
        if config.general.fn_layer == name {
            roles.push("shown while holding Fn".to_string());
        }
        for layer in &config.layers {
            for b in &layer.buttons {
                if b.open.as_deref() == Some(name) {
                    roles.push(format!("group opened from “{}”", layer.name));
                } else if b.switch.as_deref() == Some(name) {
                    roles.push(format!("switched to from “{}”", layer.name));
                }
            }
        }
        roles.dedup();
        if roles.is_empty() {
            "not used yet: no button opens it".into()
        } else {
            roles.join(", ")
        }
    }

    fn refresh(self: &Rc<Self>) {
        self.preview.queue_draw();
        self.undo.set_sensitive(!self.history.borrow().is_empty());

        // Breadcrumbs: Layer › group › group
        while let Some(child) = self.crumbs.first_child() {
            self.crumbs.remove(&child);
        }
        let nav = self.nav.borrow().clone();
        for (depth, name) in nav.iter().enumerate() {
            let crumb = gtk::Button::with_label(name);
            if depth == nav.len() - 1 {
                crumb.add_css_class("suggested-action");
            }
            let ui = self.clone();
            crumb.connect_clicked(move |_| {
                ui.nav.borrow_mut().truncate(depth + 1);
                ui.refresh();
            });
            self.crumbs.append(&crumb);
        }

        let current = self.current();
        self.info.set_label(&format!("Editing “{current}”: {}", self.role(&current)));

        // Button rows
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let buttons = self.current_buttons();
        let last = buttons.len().saturating_sub(1);
        for (i, b) in buttons.iter().enumerate() {
            self.list.append(&self.button_row(b, i, last));
        }

        // Layer rows
        while let Some(child) = self.layers.first_child() {
            self.layers.remove(&child);
        }
        let layers: Vec<Layer> = self.config.borrow().layers.clone();
        for layer in &layers {
            self.layers.append(&self.layer_row(layer));
        }
    }

    fn button_row(self: &Rc<Self>, b: &Button, index: usize, last: usize) -> adw::ActionRow {
        let kind = kind_of(b);
        let title = match kind {
            Kind::Clock => "Clock".to_string(),
            Kind::Battery => "Battery".to_string(),
            Kind::Spacer => "Empty space".to_string(),
            _ => b
                .label
                .clone()
                .filter(|l| l.chars().any(char::is_alphanumeric))
                .or_else(|| (!b.key.is_empty()).then(|| b.key.join(" + ")))
                .or_else(|| b.open.as_ref().map(|g| format!("Group: {g}")))
                .or_else(|| b.switch.as_ref().map(|l| format!("Go to {l}")))
                .or_else(|| b.icon.as_ref().map(|i| i.trim_end_matches("-symbolic").replace('-', " ")))
                .unwrap_or_else(|| "Button".into()),
        };
        let mut details = Vec::new();
        match kind {
            Kind::Key if !b.key.is_empty() => details.push(format!("presses {}", b.key.join(" + "))),
            Kind::Key => details.push("does nothing yet".into()),
            Kind::Group => details.push(format!("opens group “{}”", b.open.clone().unwrap_or_default())),
            Kind::Switch => details.push(format!("switches to “{}”", b.switch.clone().unwrap_or_default())),
            Kind::Clock => details.push(b.clock.clone().unwrap_or_default()),
            Kind::Battery => details.push(b.battery.clone().unwrap_or_default()),
            Kind::Spacer => {}
        }
        if b.width() > 1 {
            details.push(format!("width ×{}", b.width()));
        }
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(details.join(" · "))
            .use_markup(false)
            .build();
        if let Some(icon) = &b.icon {
            let image = if icon.starts_with('/') {
                gtk::Image::from_file(icon)
            } else {
                gtk::Image::from_icon_name(icon)
            };
            image.set_pixel_size(20);
            row.add_prefix(&image);
        }

        let suffix = |icon: &str, tooltip: &str, enabled: bool| {
            let button = gtk::Button::from_icon_name(icon);
            button.set_tooltip_text(Some(tooltip));
            button.set_valign(gtk::Align::Center);
            button.add_css_class("flat");
            button.set_sensitive(enabled);
            row.add_suffix(&button);
            button
        };
        if let Some(target) = b.open.clone() {
            let open = gtk::Button::with_label("Open Group");
            open.set_valign(gtk::Align::Center);
            row.add_suffix(&open);
            let ui = self.clone();
            open.connect_clicked(move |_| ui.open_group(&target));
        }
        let left = suffix("go-previous-symbolic", "Move left", index > 0);
        let right = suffix("go-next-symbolic", "Move right", index < last);
        let edit = suffix("document-edit-symbolic", "Edit", true);
        let remove = suffix("user-trash-symbolic", "Remove", true);

        let ui = self.clone();
        left.connect_clicked(move |_| ui.edit_current(|b| b.swap(index, index - 1)));
        let ui = self.clone();
        right.connect_clicked(move |_| ui.edit_current(|b| b.swap(index, index + 1)));
        let ui = self.clone();
        edit.connect_clicked(move |_| ui.edit_dialog(Some(index), kind));
        let ui = self.clone();
        remove.connect_clicked(move |_| ui.remove_button(index));
        row
    }

    fn remove_button(self: &Rc<Self>, index: usize) {
        let buttons = self.current_buttons();
        if buttons.len() == 1 {
            self.toast("A layer needs at least one button");
            return;
        }
        let target = buttons[index].open.clone();
        let name = self.current();
        self.change(|c| {
            if let Some(layer) = c.layers.iter_mut().find(|l| l.name == name) {
                layer.buttons.remove(index);
            }
            // Removing a group button removes its group, unless something
            // else still uses that layer.
            if let Some(t) = target {
                let special = c.general.default_layer == t || c.general.fn_layer == t;
                if !special && !c.is_referenced(&t) {
                    c.layers.retain(|l| l.name != t);
                }
            }
        });
    }

    fn layer_row(self: &Rc<Self>, layer: &Layer) -> adw::ActionRow {
        let name = layer.name.clone();
        let row = adw::ActionRow::builder()
            .title(&name)
            .subtitle(format!("{} buttons · {}", layer.buttons.len(), self.role(&name)))
            .use_markup(false)
            .build();
        let config = self.config.borrow();
        let removable = config.general.default_layer != name
            && config.general.fn_layer != name
            && !config.is_referenced(&name);
        drop(config);

        let edit = gtk::Button::with_label("Edit");
        edit.set_valign(gtk::Align::Center);
        let rename = gtk::Button::from_icon_name("document-edit-symbolic");
        rename.set_tooltip_text(Some("Rename"));
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_tooltip_text(Some(if removable {
            "Delete"
        } else {
            "In use: it can't be deleted"
        }));
        delete.set_sensitive(removable);
        for b in [&rename, &delete] {
            b.set_valign(gtk::Align::Center);
            b.add_css_class("flat");
        }
        row.add_suffix(&edit);
        row.add_suffix(&rename);
        row.add_suffix(&delete);

        let ui = self.clone();
        let n = name.clone();
        edit.connect_clicked(move |_| ui.set_root(&n));
        let ui = self.clone();
        let n = name.clone();
        rename.connect_clicked(move |_| ui.rename_dialog(&n));
        let ui = self.clone();
        delete.connect_clicked(move |_| {
            let n = name.clone();
            ui.change(move |c| c.layers.retain(|l| l.name != n));
        });
        row
    }

    fn new_layer(self: &Rc<Self>) {
        let name = self.config.borrow().unused_layer_name("layer");
        let n = name.clone();
        self.change(move |c| {
            c.layers.push(Layer {
                name: n,
                buttons: vec![Button {
                    label: Some("New".into()),
                    ..Default::default()
                }],
            })
        });
        self.set_root(&name);
        self.toast("New layer added — add a “Switch to another layer” button somewhere to reach it");
    }

    fn rename_dialog(self: &Rc<Self>, old: &str) {
        let entry = adw::EntryRow::builder().title("Name").build();
        entry.set_text(old);
        let group = adw::PreferencesGroup::new();
        group.add(&entry);
        let error = gtk::Label::new(None);
        error.add_css_class("error");
        let (dialog, save) = self.dialog("Rename Layer", "Rename", &[group.upcast_ref(), error.upcast_ref()]);
        let ui = self.clone();
        let old = old.to_string();
        save.connect_clicked(move |_| {
            let new = entry.text().trim().to_string();
            if new.is_empty() {
                error.set_label("The name can't be empty");
                return;
            }
            if new != old && ui.config.borrow().layer(&new).is_some() {
                error.set_label("Another layer already has that name");
                return;
            }
            let (o, n) = (old.clone(), new.clone());
            ui.change(move |c| rename_layer(c, &o, &n));
            for entry in ui.nav.borrow_mut().iter_mut() {
                if *entry == old {
                    *entry = new.clone();
                }
            }
            ui.refresh();
            dialog.close();
        });
    }

    /// A dialog with Cancel and a confirm button around `children`.
    fn dialog(&self, title: &str, confirm: &str, children: &[&gtk::Widget]) -> (adw::Dialog, gtk::Button) {
        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::with_label(confirm);
        ok.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&ok);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(12);
        body.set_margin_bottom(18);
        body.set_margin_start(12);
        body.set_margin_end(12);
        for child in children {
            body.append(*child);
        }
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .child(&body)
                .build(),
        ));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(520)
            .child(&view)
            .build();
        let d = dialog.clone();
        cancel.connect_clicked(move |_| {
            d.close();
        });
        dialog.present(Some(&self.window));
        (dialog, ok)
    }

    // ----- button editor -----

    fn edit_dialog(self: &Rc<Self>, index: Option<usize>, preset: Kind) {
        let original = match index {
            Some(i) => self.current_buttons()[i].clone(),
            None => Button::default(),
        };
        let layer_names: Vec<String> = self.config.borrow().layers.iter().map(|l| l.name.clone()).collect();

        let kind = adw::ComboRow::builder()
            .title("Type")
            .model(&gtk::StringList::new(&KINDS.map(|(_, l)| l)))
            .build();
        kind.set_selected(KINDS.iter().position(|(k, _)| *k == preset).unwrap_or(0) as u32);

        let label = adw::EntryRow::builder().title("Label (optional)").build();
        label.set_text(original.label.as_deref().unwrap_or_default());

        let icon = adw::EntryRow::builder().title("Icon (optional)").build();
        icon.set_text(original.icon.as_deref().unwrap_or_default());
        let icon_preview = gtk::Image::new();
        icon_preview.set_pixel_size(20);
        icon.add_prefix(&icon_preview);
        let choose_icon = gtk::Button::with_label("Choose…");
        choose_icon.set_valign(gtk::Align::Center);
        icon.add_suffix(&choose_icon);
        {
            let preview = icon_preview.clone();
            let update = move |e: &adw::EntryRow| {
                let t = e.text();
                if t.starts_with('/') {
                    preview.set_from_file(Some(t.as_str()));
                } else {
                    preview.set_icon_name(Some(t.as_str()));
                }
            };
            update(&icon);
            icon.connect_changed(update);
        }
        {
            let ui = self.clone();
            let icon = icon.clone();
            choose_icon.connect_clicked(move |_| {
                let icon = icon.clone();
                ui.icon_picker(move |name| icon.set_text(&name));
            });
        }
        let tint = adw::SwitchRow::builder()
            .title("White icon")
            .subtitle("Turn off for colourful icons")
            .active(original.tint.unwrap_or(true))
            .build();

        let keys = adw::EntryRow::builder().title("Key to press (combos: LeftCtrl+C)").build();
        keys.set_text(&original.key.join("+"));
        let mut key_choices = vec!["Pick a key…"];
        key_choices.extend(self.keys.iter().map(String::as_str));
        let key_picker = adw::ComboRow::builder()
            .title("Pick a key")
            .subtitle("Adds to the combo if the field above ends with +")
            .model(&gtk::StringList::new(&key_choices))
            .enable_search(true)
            .expression(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                gtk::Expression::NONE,
                "string",
            ))
            .build();
        {
            let keys = keys.clone();
            key_picker.connect_selected_notify(move |picker| {
                if picker.selected() == 0 {
                    return;
                }
                let Some(key) = picker.selected_item().and_downcast::<gtk::StringObject>() else { return };
                let current = keys.text();
                if current.trim_end().ends_with('+') {
                    keys.set_text(&format!("{}{}", current.trim_end(), key.string()));
                } else {
                    keys.set_text(&key.string());
                }
                picker.set_selected(0);
            });
        }

        let group_name = adw::EntryRow::builder().title("Group name").build();
        group_name.set_text(
            &original
                .open
                .clone()
                .unwrap_or_else(|| self.config.borrow().unused_layer_name("group")),
        );

        let switch_refs: Vec<&str> = layer_names.iter().map(String::as_str).collect();
        let switch_target = adw::ComboRow::builder()
            .title("Layer to switch to")
            .model(&gtk::StringList::new(&switch_refs))
            .build();
        if let Some(t) = &original.switch {
            if let Some(p) = layer_names.iter().position(|n| n == t) {
                switch_target.set_selected(p as u32);
            }
        }

        let clock = adw::ComboRow::builder()
            .title("Clock")
            .model(&gtk::StringList::new(&["24-hour", "12-hour", "Custom"]))
            .build();
        let clock_format = adw::EntryRow::builder()
            .title("Format (e.g. %H:%M  %a %-d %b)")
            .build();
        match original.clock.as_deref() {
            Some("12h") => clock.set_selected(1),
            Some("24h") | None => clock.set_selected(0),
            Some(custom) => {
                clock.set_selected(2);
                clock_format.set_text(custom);
            }
        }

        let battery = adw::ComboRow::builder()
            .title("Battery")
            .model(&gtk::StringList::new(&["Icon and percentage", "Percentage", "Icon"]))
            .build();
        battery.set_selected(match original.battery.as_deref() {
            Some("percent") => 1,
            Some("icon") => 2,
            _ => 0,
        });

        let width = adw::SpinRow::with_range(1.0, 24.0, 1.0);
        width.set_title("Width");
        width.set_subtitle("Button spaces it takes up");
        width.set_value(original.width() as f64);

        let error = gtk::Label::new(None);
        error.add_css_class("error");
        error.set_wrap(true);

        let group = adw::PreferencesGroup::new();
        for row in [
            kind.upcast_ref::<gtk::Widget>(),
            group_name.upcast_ref(),
            switch_target.upcast_ref(),
            clock.upcast_ref(),
            clock_format.upcast_ref(),
            battery.upcast_ref(),
            label.upcast_ref(),
            icon.upcast_ref(),
            tint.upcast_ref(),
            keys.upcast_ref(),
            key_picker.upcast_ref(),
            width.upcast_ref(),
        ] {
            group.add(row);
        }

        let update = {
            let (kind, group_name, switch_target, clock, clock_format, battery) = (
                kind.clone(),
                group_name.clone(),
                switch_target.clone(),
                clock.clone(),
                clock_format.clone(),
                battery.clone(),
            );
            let (label, icon, tint, keys, key_picker) =
                (label.clone(), icon.clone(), tint.clone(), keys.clone(), key_picker.clone());
            move || {
                let k = KINDS[kind.selected() as usize % KINDS.len()].0;
                let looks = matches!(k, Kind::Key | Kind::Group | Kind::Switch);
                group_name.set_visible(k == Kind::Group);
                switch_target.set_visible(k == Kind::Switch);
                clock.set_visible(k == Kind::Clock);
                clock_format.set_visible(k == Kind::Clock && clock.selected() == 2);
                battery.set_visible(k == Kind::Battery);
                label.set_visible(looks);
                icon.set_visible(looks);
                tint.set_visible(looks);
                keys.set_visible(k == Kind::Key);
                key_picker.set_visible(k == Kind::Key);
            }
        };
        update();
        {
            let f = update.clone();
            kind.connect_selected_notify(move |_| f());
            clock.connect_selected_notify(move |_| update());
        }

        let title = if index.is_some() { "Edit Button" } else { "Add Button" };
        let confirm = if index.is_some() { "Save" } else { "Add" };
        let (dialog, ok) = self.dialog(title, confirm, &[group.upcast_ref(), error.upcast_ref()]);

        let ui = self.clone();
        ok.connect_clicked(move |_| {
            let k = KINDS[kind.selected() as usize % KINDS.len()].0;
            let text = |e: &adw::EntryRow| {
                let t = e.text().trim().to_string();
                (!t.is_empty()).then_some(t)
            };
            let mut button = Button::default();
            if matches!(k, Kind::Key | Kind::Group | Kind::Switch) {
                button.label = text(&label);
                button.icon = text(&icon);
                if !tint.is_active() {
                    button.tint = Some(false);
                }
                if button.label.is_none() && button.icon.is_none() {
                    error.set_label("Give the button a label or an icon");
                    return;
                }
            }
            let mut new_group = None;
            match k {
                Kind::Key => {
                    button.key = keys
                        .text()
                        .split('+')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    if let Some(bad) = button.key.iter().find(|k| config::parse_key(k).is_none()) {
                        error.set_label(&format!("“{bad}” is not a key name. Use “Pick a key”."));
                        return;
                    }
                }
                Kind::Group => {
                    let Some(name) = text(&group_name) else {
                        error.set_label("Give the group a name");
                        return;
                    };
                    let exists = ui.config.borrow().layer(&name).is_some();
                    if !exists {
                        new_group = Some(name.clone());
                    } else if original.open.as_deref() != Some(&name) {
                        // Opening an existing layer as a group is allowed.
                    }
                    button.open = Some(name);
                }
                Kind::Switch => {
                    button.switch = layer_names.get(switch_target.selected() as usize).cloned();
                }
                Kind::Clock => {
                    button.clock = Some(match clock.selected() {
                        0 => "24h".into(),
                        1 => "12h".into(),
                        _ => clock_format.text().trim().to_string(),
                    });
                    if render::clock_text(button.clock.as_deref().unwrap()).is_none() {
                        error.set_label("That clock format isn't valid");
                        return;
                    }
                }
                Kind::Battery => {
                    button.battery = Some(["both", "percent", "icon"][battery.selected().min(2) as usize].into());
                }
                Kind::Spacer => {}
            }
            let w = width.value() as u32;
            if w > 1 || original.width.is_some() {
                button.width = Some(w);
            }

            let name = ui.current();
            ui.change(|c| {
                if let Some(group) = &new_group {
                    c.layers.push(Layer {
                        name: group.clone(),
                        buttons: vec![Button {
                            label: Some("New".into()),
                            ..Default::default()
                        }],
                    });
                }
                if let Some(layer) = c.layers.iter_mut().find(|l| l.name == name) {
                    match index {
                        Some(i) => layer.buttons[i] = button,
                        None => layer.buttons.push(button),
                    }
                }
            });
            if let Some(group) = new_group {
                ui.open_group(&group);
                ui.toast("Group created — add its buttons here; the back button returns");
            }
            dialog.close();
        });
    }

    fn icon_picker(self: &Rc<Self>, on_pick: impl Fn(String) + 'static) {
        let names = self
            .icon_names
            .borrow_mut()
            .get_or_insert_with(|| self.icons.borrow_mut().names())
            .clone();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let model = gtk::StringList::new(&refs);
        let filter = gtk::StringFilter::new(Some(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            gtk::Expression::NONE,
            "string",
        )));
        filter.set_match_mode(gtk::StringFilterMatchMode::Substring);
        let filtered = gtk::FilterListModel::new(Some(model), Some(filter.clone()));
        let selection = gtk::NoSelection::new(Some(filtered));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
            cell.set_margin_top(6);
            cell.set_margin_bottom(6);
            let image = gtk::Image::new();
            image.set_pixel_size(28);
            let label = gtk::Label::new(None);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            label.set_max_width_chars(14);
            label.add_css_class("caption");
            cell.append(&image);
            cell.append(&label);
            item.set_child(Some(&cell));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let name = item.item().and_downcast::<gtk::StringObject>().unwrap().string();
            let cell = item.child().and_downcast::<gtk::Box>().unwrap();
            let image = cell.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = cell.last_child().and_downcast::<gtk::Label>().unwrap();
            image.set_icon_name(Some(&name));
            label.set_text(name.trim_end_matches("-symbolic"));
            cell.set_tooltip_text(Some(&name));
        });
        let grid = gtk::GridView::builder()
            .model(&selection)
            .factory(&factory)
            .max_columns(8)
            .single_click_activate(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(420)
            .child(&grid)
            .build();
        let search = gtk::SearchEntry::builder().placeholder_text("Search icons").build();
        search.connect_search_changed(move |s| filter.set_search(Some(s.text().as_str())));

        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.set_margin_top(8);
        body.set_margin_bottom(8);
        body.set_margin_start(8);
        body.set_margin_end(8);
        body.append(&search);
        body.append(&scroller);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title("Choose an Icon")
            .content_width(760)
            .content_height(600)
            .child(&view)
            .build();
        {
            let dialog = dialog.clone();
            grid.connect_activate(move |grid, position| {
                let name = grid
                    .model()
                    .and_then(|m| m.item(position))
                    .and_downcast::<gtk::StringObject>()
                    .map(|s| s.string().to_string());
                if let Some(name) = name {
                    on_pick(name);
                }
                dialog.close();
            });
        }
        dialog.present(Some(&self.window));
        search.grab_focus();
    }
}

/// Renames a layer and every reference to it.
fn rename_layer(c: &mut Config, old: &str, new: &str) {
    for layer in &mut c.layers {
        if layer.name == old {
            layer.name = new.to_string();
        }
        for b in &mut layer.buttons {
            for target in [&mut b.open, &mut b.switch].into_iter().flatten() {
                if target == old {
                    *target = new.to_string();
                }
            }
        }
    }
    for g in [&mut c.general.default_layer, &mut c.general.fn_layer] {
        if g == old {
            *g = new.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_updates_references() {
        let mut c = Config::parse(config::DEFAULT_CONFIG).unwrap();
        rename_layer(&mut c, "brightness", "display");
        rename_layer(&mut c, "media", "main");
        assert!(c.layer("display").is_some());
        assert_eq!(c.layers[0].buttons[0].open.as_deref(), Some("display"));
        assert_eq!(c.general.default_layer, "main");
        c.check().unwrap();
    }

    #[test]
    fn kinds_are_detected() {
        let c = Config::parse(config::DEFAULT_CONFIG).unwrap();
        let media = &c.layer("media").unwrap().buttons;
        assert_eq!(kind_of(&media[0]), Kind::Group);
        assert_eq!(kind_of(&media[1]), Kind::Key);
        assert!(media.iter().any(|b| kind_of(b) == Kind::Clock));
        assert!(media.iter().any(|b| kind_of(b) == Kind::Battery));
        assert!(media.iter().any(|b| kind_of(b) == Kind::Spacer));
    }
}
