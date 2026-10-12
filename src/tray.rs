//! Menu-bar (status) item. As a quake accessory app stdusk has no Dock icon, so this is its
//! presence + control surface: a monochrome template icon (macOS tints it for light/dark menu
//! bars) with a Show/Hide + Quit menu. Menu clicks are polled from `MenuEvent::receiver()` each
//! frame, the same pattern as the global hotkey. macOS only: tray-icon needs a GTK event loop on
//! Linux, which this app does not run, so other platforms get an inert `Tray` that is never built.
#[cfg(target_os = "macos")]
use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
#[cfg(target_os = "macos")]
use tray_icon::{TrayIcon, TrayIconBuilder};

#[cfg(target_os = "macos")]
pub(crate) struct Tray {
    _icon: TrayIcon, // kept alive; dropping it removes the menu-bar item
    show: MenuId,
    quit: MenuId,
    restart: MenuItem, // handle kept (not just its id) so its label can name a pending version
}

/// Build the menu-bar item, or `None` if the platform/init won't allow it.
#[cfg(target_os = "macos")]
pub(crate) fn build() -> Option<Tray> {
    let data =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/stdusk-tray.png")).ok()?;
    let icon = tray_icon::Icon::from_rgba(data.rgba, data.width, data.height).ok()?;

    let menu = Menu::new();
    let show = MenuItem::new("Show / Hide", true, None);
    let restart = MenuItem::new(RESTART_PLAIN, true, None);
    let quit = MenuItem::new("Quit stdusk", true, None);
    menu.append(&show).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&restart).ok()?;
    menu.append(&quit).ok()?;
    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_icon_as_template(true) // macOS auto-tints template icons for the menu-bar appearance
        .with_tooltip("stdusk")
        .with_menu_on_left_click(true)
        .build()
        .ok()?;

    Some(Tray { _icon: icon, show: show_id, quit: quit_id, restart })
}

/// Label when nothing new is installed - a restart is still useful on its own.
#[cfg(target_os = "macos")]
const RESTART_PLAIN: &str = "Restart stdusk";

/// Point the Restart item at a pending version, or back to the plain label. Called only when the
/// pending state CHANGES: the tray menu is built once at launch, but a `brew reinstall` can land
/// while we run, so the label has to be able to catch up.
#[cfg(target_os = "macos")]
pub(crate) fn set_pending_update(tray: &Tray, pending: Option<&str>) {
    match pending {
        Some(v) => tray.restart.set_text(format!("Restart to update to {v}")),
        None => tray.restart.set_text(RESTART_PLAIN),
    }
}

/// Drain menu clicks since the last frame.
pub(crate) struct TrayClicks {
    pub(crate) show_hide: bool,
    pub(crate) quit: bool,
    pub(crate) restart: bool,
}

#[cfg(target_os = "macos")]
pub(crate) fn poll(tray: &Tray) -> TrayClicks {
    let mut c = TrayClicks { show_hide: false, quit: false, restart: false };
    while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
        if ev.id == tray.show {
            c.show_hide = true;
        } else if ev.id == tray.quit {
            c.quit = true;
        } else if ev.id == *tray.restart.id() {
            c.restart = true;
        }
    }
    c
}

#[cfg(not(target_os = "macos"))]
pub(crate) enum Tray {}

#[cfg(not(target_os = "macos"))]
pub(crate) fn build() -> Option<Tray> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_pending_update(tray: &Tray, _pending: Option<&str>) {
    match *tray {}
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn poll(tray: &Tray) -> TrayClicks {
    match *tray {}
}
