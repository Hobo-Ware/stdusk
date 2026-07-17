//! Menu-bar (status) item. As a quake accessory app stdusk has no Dock icon, so this is its
//! presence + control surface: a monochrome template icon (macOS tints it for light/dark menu
//! bars) with a Show/Hide + Quit menu. Menu clicks are polled from `MenuEvent::receiver()` each
//! frame, the same pattern as the global hotkey.
use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub(crate) struct Tray {
    _icon: TrayIcon, // kept alive; dropping it removes the menu-bar item
    show: MenuId,
    quit: MenuId,
}

/// Build the menu-bar item, or `None` if the platform/init won't allow it.
pub(crate) fn build() -> Option<Tray> {
    let data =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/stdusk-tray.png")).ok()?;
    let icon = tray_icon::Icon::from_rgba(data.rgba, data.width, data.height).ok()?;

    let menu = Menu::new();
    let show = MenuItem::new("Show / Hide", true, None);
    let quit = MenuItem::new("Quit stdusk", true, None);
    menu.append(&show).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
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

    Some(Tray { _icon: icon, show: show_id, quit: quit_id })
}

/// Drain menu clicks since the last frame: `(show_hide_clicked, quit_clicked)`.
pub(crate) fn poll(tray: &Tray) -> (bool, bool) {
    let (mut show, mut quit) = (false, false);
    while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
        if ev.id == tray.show {
            show = true;
        } else if ev.id == tray.quit {
            quit = true;
        }
    }
    (show, quit)
}
