//! Clipboard access for the GUI.
//!
//! Copying goes only through egui (`Context::copy_text`), which uses the window's own
//! long-lived clipboard connection (Wayland or X11/Windows as appropriate). The GUI used
//! to also open a fresh `arboard` clipboard for every copy. On Linux that meant a new X11
//! connection, window and selection owner per click, torn down again immediately. Under
//! WSLg the compositor bridges the X11 and Wayland clipboards, and rapid churn there can
//! make it drop the app's connection, closing the window.
//!
//! Reading still needs `arboard` (egui only delivers clipboard text through paste
//! keystrokes), so the Paste menu items share one instance for the whole session.

use std::cell::RefCell;

pub fn copy(ctx: &egui::Context, text: impl Into<String>) {
    ctx.copy_text(text.into());
}

/// Current clipboard text, for menu-driven paste.
pub fn read_text() -> Option<String> {
    thread_local! {
        static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    }
    CLIPBOARD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        slot.as_mut()?.get_text().ok()
    })
}
