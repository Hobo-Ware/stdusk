//! The terminal grid as the keyboard target. While nothing else wants the keys it holds egui's
//! focus with Tab, Shift+Tab, the arrows and Esc filtered to it, so they reach the shell as bytes
//! and never walk keyboard focus across the buttons around it.

use egui::{Context, EventFilter, Id};

const KEYS_TO_THE_SHELL: EventFilter =
    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };

fn grid_id_key() -> Id {
    Id::new("terminal-grid-widget")
}

fn keys_belong_to_the_shell(ctx: &Context, keys_free: bool) -> bool {
    keys_free && !egui::Popup::is_any_open(ctx)
}

/// Take egui's focus for `grid` when nothing has it, give it up when the keys are not free, and
/// filter the shell's keys to it while they are.
pub fn hold(ctx: &Context, grid: Id, keys_free: bool) {
    ctx.data_mut(|data| data.insert_temp(grid_id_key(), grid));
    let keys_free = keys_belong_to_the_shell(ctx, keys_free);
    ctx.memory_mut(|memory| {
        match (keys_free, memory.focused()) {
            (true, None) => memory.request_focus(grid),
            (false, Some(held)) if held == grid => memory.surrender_focus(grid),
            _ => {}
        }
        if keys_free {
            memory.set_focus_lock_filter(grid, KEYS_TO_THE_SHELL);
        }
    });
}

/// Only the key filter, for an app that requests focus for the grid itself.
pub fn lock_keys(ctx: &Context, grid: Id, keys_free: bool) {
    if keys_belong_to_the_shell(ctx, keys_free) {
        ctx.memory_mut(|memory| memory.set_focus_lock_filter(grid, KEYS_TO_THE_SHELL));
    }
}

/// Whether `id` is the grid last held through [`hold`].
pub fn is_grid(ctx: &Context, id: Id) -> bool {
    ctx.data(|data| data.get_temp::<Id>(grid_id_key())) == Some(id)
}
