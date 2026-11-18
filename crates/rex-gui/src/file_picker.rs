//! Built-in file picker for systems without a usable system file dialog.
//!
//! On Linux the GUI's Open/Save dialogs go through `rfd`, which needs either the XDG
//! desktop portal or `zenity`. Minimal installs and WSL2 often have neither, and `rfd`
//! then returns exactly as if the user had cancelled. In that case the GUI shows this
//! picker instead: a path box with live autocomplete and a listing of the folder being
//! typed, sharing the TUI's shell-style Tab completion.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{Key, Modifiers, RichText};

use crate::paths::{complete_path, expand_path};

/// A system dialog that returns "cancelled" faster than this never reached the screen.
pub const SYSTEM_DIALOG_MIN_HUMAN_TIME: Duration = Duration::from_millis(300);

/// Forces the built-in picker when set to `1` (useful where a system dialog exists but
/// misbehaves).
pub const FORCE_BUILTIN_ENV: &str = "REXEDITOR_BUILTIN_FILE_PICKER";

/// Whether a system file dialog can be shown. Linux needs `zenity` or a running XDG
/// desktop portal; other platforms always have one.
pub fn system_dialog_available() -> bool {
    if std::env::var(FORCE_BUILTIN_ENV).is_ok_and(|v| v == "1") {
        return false;
    }
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return true;
    }
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
    };
    let session_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty());
    let portal_installed = [
        "/usr/libexec/xdg-desktop-portal",
        "/usr/lib/xdg-desktop-portal",
        "/usr/lib/x86_64-linux-gnu/xdg-desktop-portal",
        "/usr/lib/aarch64-linux-gnu/xdg-desktop-portal",
    ]
    .iter()
    .any(|p| Path::new(p).is_file());
    on_path("zenity") || (session_bus && portal_installed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerPurpose {
    OpenBinary,
    SaveBinaryAs,
    LoadOverlay,
    SaveOverlayAs,
}

impl PickerPurpose {
    fn title(self) -> &'static str {
        match self {
            Self::OpenBinary => "Open Binary File",
            Self::SaveBinaryAs => "Save Binary As",
            Self::LoadOverlay => "Load Overlay JSON",
            Self::SaveOverlayAs => "Save Overlay JSON As",
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::OpenBinary => "Open",
            Self::LoadOverlay => "Load",
            Self::SaveBinaryAs | Self::SaveOverlayAs => "Save",
        }
    }

    fn is_save(self) -> bool {
        matches!(self, Self::SaveBinaryAs | Self::SaveOverlayAs)
    }

    fn json_only(self) -> bool {
        matches!(self, Self::LoadOverlay | Self::SaveOverlayAs)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PickerOutcome {
    Pending,
    Chosen(PathBuf),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

pub struct FilePicker {
    pub purpose: PickerPurpose,
    pub input: String,
    /// Highlighted suggestion (keyboard or single click).
    pub selected: Option<usize>,
    message: Option<String>,
    /// A save target that exists and was confirmed once already.
    confirm_overwrite: Option<PathBuf>,
    focus_input: bool,
    cursor_to_end: bool,
    scroll_to_selected: bool,
    listing: Option<(PathBuf, Vec<Entry>)>,
}

/// Formats a directory for the path box, using `~` for the home directory and always
/// ending in `/`.
fn display_dir(dir: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut s = match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    };
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// Splits typed text into the folder part (up to the last `/`) and the name being typed.
fn split_input(input: &str) -> (&str, &str) {
    let split = input.rfind('/').map_or(0, |i| i + 1);
    input.split_at(split)
}

impl FilePicker {
    /// `dir` is where browsing starts; `file_name` pre-fills the name for saves.
    pub fn new(purpose: PickerPurpose, dir: &Path, file_name: Option<&str>) -> Self {
        let mut input = display_dir(dir);
        if let Some(name) = file_name {
            input.push_str(name);
        }
        Self {
            purpose,
            input,
            selected: None,
            message: None,
            confirm_overwrite: None,
            focus_input: true,
            cursor_to_end: true,
            scroll_to_selected: false,
            listing: None,
        }
    }

    /// Entries of the folder currently typed, filtered by the partial name. Directories
    /// come first; dotfiles only show once a `.` is typed; overlay pickers list only
    /// folders and `.json` files.
    pub fn suggestions(&mut self) -> Vec<Entry> {
        let (dir_part, name_part) = split_input(&self.input);
        let dir = if dir_part.is_empty() {
            PathBuf::from(".")
        } else {
            expand_path(dir_part)
        };
        let name_part = name_part.to_string();
        if self.listing.as_ref().is_none_or(|(d, _)| *d != dir) {
            let mut entries: Vec<Entry> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| {
                            Some(Entry {
                                name: e.file_name().into_string().ok()?,
                                is_dir: e.path().is_dir(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            self.listing = Some((dir, entries));
        }
        let json_only = self.purpose.json_only();
        let entries = &self.listing.as_ref().expect("listing was just filled").1;
        entries
            .iter()
            .filter(|e| e.name.starts_with(&name_part))
            .filter(|e| !e.name.starts_with('.') || name_part.starts_with('.'))
            .filter(|e| e.is_dir || !json_only || e.name.to_lowercase().ends_with(".json"))
            .cloned()
            .collect()
    }

    fn set_input(&mut self, text: String) {
        self.input = text;
        self.selected = None;
        self.message = None;
        self.confirm_overwrite = None;
        self.cursor_to_end = true;
        self.focus_input = true;
    }

    /// Shell-style Tab: fill in the longest common prefix of the matches.
    pub fn tab_complete(&mut self) {
        let done = complete_path(&self.input);
        if done.text != self.input {
            self.set_input(done.text);
        } else if done.matches.len() > 1 {
            self.message = Some(format!("{} matches", done.matches.len()));
        }
    }

    pub fn go_up(&mut self) {
        let (dir_part, _) = split_input(&self.input);
        let dir = if dir_part.is_empty() {
            PathBuf::from(".")
        } else {
            expand_path(dir_part)
        };
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if let Some(parent) = dir.parent() {
            self.set_input(display_dir(parent));
        }
    }

    /// Opens a folder entry, or puts a file entry in the path box (and accepts it when
    /// `accept` is set, e.g. Enter or double-click).
    pub fn choose(&mut self, entry: &Entry, accept: bool) -> PickerOutcome {
        let (dir_part, _) = split_input(&self.input);
        let mut text = format!("{dir_part}{}", entry.name);
        if entry.is_dir {
            text.push('/');
            self.set_input(text);
            return PickerOutcome::Pending;
        }
        self.set_input(text);
        if accept {
            self.accept()
        } else {
            PickerOutcome::Pending
        }
    }

    /// Validates the typed path for this picker's purpose.
    pub fn accept(&mut self) -> PickerOutcome {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.message = Some("Type a path".into());
            return PickerOutcome::Pending;
        }
        let path = expand_path(&text);
        if path.is_dir() {
            if self.purpose.is_save() {
                self.message = Some("That is a folder: type a file name after it".into());
            } else {
                let dir = if text.ends_with('/') {
                    text
                } else {
                    format!("{text}/")
                };
                self.set_input(dir);
            }
            return PickerOutcome::Pending;
        }
        if !self.purpose.is_save() {
            if path.is_file() {
                return PickerOutcome::Chosen(path);
            }
            self.message = Some(format!("No such file: {text}"));
            return PickerOutcome::Pending;
        }
        let parent_ok = path
            .parent()
            .is_none_or(|p| p.as_os_str().is_empty() || p.is_dir());
        if !parent_ok {
            self.message = Some("That folder does not exist".into());
            return PickerOutcome::Pending;
        }
        if path.exists() && self.confirm_overwrite.as_ref() != Some(&path) {
            self.message = Some(format!(
                "{} already exists. {} again to overwrite it.",
                path.file_name()
                    .map_or_else(|| text.clone(), |n| n.to_string_lossy().into()),
                self.purpose.action()
            ));
            self.confirm_overwrite = Some(path);
            return PickerOutcome::Pending;
        }
        PickerOutcome::Chosen(path)
    }

    fn move_selection(&mut self, down: bool, count: usize) {
        if count == 0 {
            self.selected = None;
            return;
        }
        self.selected = Some(match (self.selected, down) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(i), true) => (i + 1).min(count - 1),
            (Some(i), false) => i.saturating_sub(1),
        });
        self.scroll_to_selected = true;
    }

    /// Draws the picker as a modal and returns what the user did this frame.
    pub fn show(&mut self, ctx: &egui::Context) -> PickerOutcome {
        let (tab, down, up, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if tab {
            self.tab_complete();
        }
        let suggestions = self.suggestions();
        if down || up {
            self.move_selection(down, suggestions.len());
        }
        if self.selected.is_some_and(|i| i >= suggestions.len()) {
            self.selected = None;
        }
        let mut outcome = PickerOutcome::Pending;
        if enter {
            outcome = match self.selected.and_then(|i| suggestions.get(i)).cloned() {
                Some(entry) => self.choose(&entry, true),
                None => self.accept(),
            };
        }

        let input_id = egui::Id::new("rex_file_picker_input");
        if std::mem::take(&mut self.cursor_to_end)
            && let Some(mut state) = egui::TextEdit::load_state(ctx, input_id)
        {
            let end = egui::text::CCursor::new(self.input.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ctx, input_id);
        }

        let modal = egui::Modal::new(egui::Id::new("rex_file_picker")).show(ctx, |ui| {
            ui.set_width(620.0_f32.min(ctx.screen_rect().width() - 40.0));
            ui.heading(self.purpose.title());
            ui.label(
                RichText::new(
                    "No working system file dialog was found, so RexEditor is using its \
                     built-in picker. On Linux, installing `zenity` enables the system dialog.",
                )
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("⬆").on_hover_text("Parent folder").clicked() {
                    self.go_up();
                }
                if ui.button("🏠").on_hover_text("Home folder").clicked() {
                    self.set_input("~/".into());
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .id(input_id)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("Type a path; Tab completes")
                        // Keep Tab for completion instead of moving focus.
                        .lock_focus(true)
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_input) {
                    resp.request_focus();
                }
                if resp.changed() {
                    self.selected = None;
                    self.message = None;
                    self.confirm_overwrite = None;
                }
            });

            let mut clicked: Option<(Entry, bool)> = None;
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if suggestions.is_empty() {
                        ui.weak("No matching files or folders");
                    }
                    for (i, entry) in suggestions.iter().enumerate() {
                        let label = if entry.is_dir {
                            RichText::new(format!("{}/", entry.name))
                                .monospace()
                                .color(ui.visuals().hyperlink_color)
                        } else {
                            RichText::new(&entry.name).monospace()
                        };
                        let resp = ui.selectable_label(self.selected == Some(i), label);
                        if self.selected == Some(i) && std::mem::take(&mut self.scroll_to_selected)
                        {
                            resp.scroll_to_me(None);
                        }
                        if resp.double_clicked() {
                            clicked = Some((entry.clone(), true));
                        } else if resp.clicked() {
                            clicked = Some((entry.clone(), false));
                        }
                    }
                });
            if let Some((entry, accept)) = clicked {
                // A single click on a folder opens it; on a file it fills the path box.
                outcome = self.choose(&entry, accept);
            }

            if let Some(msg) = &self.message {
                ui.colored_label(ui.visuals().warn_fg_color, msg);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(self.purpose.action()).clicked() {
                    outcome = self.accept();
                }
                if ui.button("Cancel").clicked() {
                    outcome = PickerOutcome::Cancelled;
                }
                ui.weak("Tab completes · Up/Down choose · Enter opens · Esc cancels");
            });
        });
        if modal.should_close() && outcome == PickerOutcome::Pending {
            outcome = PickerOutcome::Cancelled;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Harness;

    /// A throwaway folder tree: a.bin, alpha.bin, notes.json, .hidden, sub/inner.bin.
    struct Tree(PathBuf);

    impl Tree {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rex-picker-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            for f in [
                "a.bin",
                "alpha.bin",
                "notes.json",
                ".hidden",
                "sub/inner.bin",
            ] {
                std::fs::write(dir.join(f), b"x").unwrap();
            }
            Self(dir)
        }

        fn picker(&self, purpose: PickerPurpose) -> FilePicker {
            let mut p = FilePicker::new(purpose, &self.0, None);
            // Use the absolute path so tests don't depend on $HOME.
            p.input = format!("{}/", self.0.display());
            p
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                if e.is_dir {
                    format!("{}/", e.name)
                } else {
                    e.name.clone()
                }
            })
            .collect()
    }

    #[test]
    fn lists_folder_with_directories_first_and_no_dotfiles() {
        let tree = Tree::new("list");
        let mut p = tree.picker(PickerPurpose::OpenBinary);
        assert_eq!(
            names(&p.suggestions()),
            ["sub/", "a.bin", "alpha.bin", "notes.json"]
        );
        p.input.push('a');
        assert_eq!(
            names(&p.suggestions()),
            ["a.bin", "alpha.bin"],
            "filters by typed name"
        );
        p.input.pop();
        p.input.push('.');
        assert_eq!(
            names(&p.suggestions()),
            [".hidden"],
            "dotfiles once a dot is typed"
        );
    }

    #[test]
    fn overlay_pickers_only_list_folders_and_json() {
        let tree = Tree::new("json");
        let mut p = tree.picker(PickerPurpose::LoadOverlay);
        assert_eq!(names(&p.suggestions()), ["sub/", "notes.json"]);
    }

    #[test]
    fn tab_completes_and_folders_are_entered() {
        let tree = Tree::new("tab");
        let base = format!("{}/", tree.0.display());
        let mut p = tree.picker(PickerPurpose::OpenBinary);
        p.input = format!("{base}al");
        p.tab_complete();
        assert_eq!(p.input, format!("{base}alpha.bin"));
        p.input = format!("{base}s");
        p.tab_complete();
        assert_eq!(p.input, format!("{base}sub/"));
        assert_eq!(names(&p.suggestions()), ["inner.bin"]);
        p.go_up();
        assert!(p.input.ends_with(&format!(
            "{}/",
            tree.0.file_name().unwrap().to_string_lossy()
        )));

        let mut p = tree.picker(PickerPurpose::OpenBinary);
        let sub = Entry {
            name: "sub".into(),
            is_dir: true,
        };
        assert_eq!(
            p.choose(&sub, true),
            PickerOutcome::Pending,
            "folders open, not accept"
        );
        assert_eq!(p.input, format!("{base}sub/"));
    }

    #[test]
    fn open_accepts_existing_files_only() {
        let tree = Tree::new("open");
        let base = format!("{}/", tree.0.display());
        let mut p = tree.picker(PickerPurpose::OpenBinary);
        p.input = format!("{base}a.bin");
        assert_eq!(p.accept(), PickerOutcome::Chosen(tree.0.join("a.bin")));
        p.input = format!("{base}missing.bin");
        assert_eq!(p.accept(), PickerOutcome::Pending);
        assert!(p.message.as_deref().unwrap().contains("No such file"));
        p.input = format!("{base}sub");
        assert_eq!(
            p.accept(),
            PickerOutcome::Pending,
            "a folder is opened instead"
        );
        assert_eq!(p.input, format!("{base}sub/"));
    }

    #[test]
    fn save_confirms_overwrite_and_checks_the_folder() {
        let tree = Tree::new("save");
        let base = format!("{}/", tree.0.display());
        let mut p = tree.picker(PickerPurpose::SaveBinaryAs);
        p.input = format!("{base}new.bin");
        assert_eq!(p.accept(), PickerOutcome::Chosen(tree.0.join("new.bin")));

        p.input = format!("{base}a.bin");
        assert_eq!(p.accept(), PickerOutcome::Pending);
        assert!(p.message.as_deref().unwrap().contains("already exists"));
        assert_eq!(
            p.accept(),
            PickerOutcome::Chosen(tree.0.join("a.bin")),
            "second Save overwrites"
        );

        p.input = format!("{base}sub/");
        assert_eq!(p.accept(), PickerOutcome::Pending);
        assert!(p.message.as_deref().unwrap().contains("type a file name"));
        p.input = format!("{base}nope/x.bin");
        assert_eq!(p.accept(), PickerOutcome::Pending);
        assert!(p.message.as_deref().unwrap().contains("does not exist"));
    }

    #[test]
    fn save_picker_prefills_the_file_name() {
        let tree = Tree::new("prefill");
        let p = FilePicker::new(PickerPurpose::SaveBinaryAs, &tree.0, Some("fw.bin"));
        assert!(p.input.ends_with("/fw.bin"));
    }

    fn frame(h: &mut Harness, p: &mut FilePicker) -> PickerOutcome {
        let mut outcome = PickerOutcome::Pending;
        h.frame(|ui| outcome = p.show(ui.ctx()));
        outcome
    }

    #[test]
    fn keyboard_choose_and_cancel_in_the_dialog() {
        let tree = Tree::new("keys");
        let mut h = Harness::new(900.0, 700.0);
        let mut p = tree.picker(PickerPurpose::OpenBinary);
        frame(&mut h, &mut p);
        // Down twice selects a.bin (after sub/), Enter opens it.
        h.key(Key::ArrowDown);
        frame(&mut h, &mut p);
        h.key(Key::ArrowDown);
        frame(&mut h, &mut p);
        assert_eq!(p.selected, Some(1));
        h.key(Key::Enter);
        assert_eq!(
            frame(&mut h, &mut p),
            PickerOutcome::Chosen(tree.0.join("a.bin"))
        );

        let mut p = tree.picker(PickerPurpose::OpenBinary);
        frame(&mut h, &mut p);
        h.key(Key::Escape);
        assert_eq!(frame(&mut h, &mut p), PickerOutcome::Cancelled);
    }

    #[test]
    fn tab_in_the_dialog_completes_and_keeps_focus() {
        let tree = Tree::new("focus");
        let base = format!("{}/", tree.0.display());
        let mut h = Harness::new(900.0, 700.0);
        let mut p = tree.picker(PickerPurpose::OpenBinary);
        p.input = format!("{base}al");
        frame(&mut h, &mut p);
        frame(&mut h, &mut p);
        h.key(Key::Tab);
        frame(&mut h, &mut p);
        assert_eq!(p.input, format!("{base}alpha.bin"));
        let input_id = egui::Id::new("rex_file_picker_input");
        assert!(
            h.ctx.memory(|m| m.has_focus(input_id)),
            "focus stays in the path box"
        );
    }
}
