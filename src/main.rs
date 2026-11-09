#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod awake;
mod centreon;
mod config;
mod fresh;
mod notify;
mod secret;
mod sound;
mod theme;

fn main() -> eframe::Result {
    // WSLg : la connexion Wayland tombe (« Broken pipe ») quand on agrandit la fenêtre, notamment sur un 2e écran ;
    // XWayland tient. MCO_WAYLAND=1 pour garder Wayland.
    if std::env::var_os("WSL_DISTRO_NAME").is_some()
        && std::env::var_os("DISPLAY").is_some()
        && std::env::var_os("MCO_WAYLAND").is_none()
    {
        // SAFETY: appelé en tout début de main, avant la création de tout thread.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
    let viewport = eframe::egui::ViewportBuilder::default().with_title("MCO Dash");
    let settings = config::Settings::load();
    let viewport = if settings.compact {
        let bar = viewport
            .with_inner_size([app::COMPACT_W, app::COMPACT_H])
            .with_min_inner_size([app::COMPACT_W, app::COMPACT_H])
            .with_decorations(false)
            .with_resizable(false);
        if settings.unpinned { bar } else { bar.with_always_on_top() }
    } else {
        viewport.with_maximized(true).with_inner_size([1440.0, 900.0]).with_min_inner_size([1000.0, 620.0])
    };
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("MCO Dash", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
