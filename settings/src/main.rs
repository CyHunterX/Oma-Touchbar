mod store;
mod window;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "io.github.cyhunterx.OmaTouchbar";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| match app.active_window() {
        Some(window) => window.present(),
        None => window::build(app),
    });
    app.run()
}
