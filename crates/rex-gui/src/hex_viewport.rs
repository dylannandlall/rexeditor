//! Virtualized hex viewport for `rex-gui`.
//!
//! Each row is painted in a single pass with `ui.painter()`, rather than allocating
//! separate widgets per byte. This keeps multi-gigabyte files fast to scroll and lets
//! a single hit-test map pointer positions to either the Hex or ASCII column.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use rex_core::{ByteBuffer, CopyFormat, DisplayRow, Document, EditKind, SearchMatch};

use crate::theme::{byte_color32, field_color32, hex_byte_str, named_color32};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditColumn {
    #[default]
    Hex,
    Ascii,
}

const FONT_SIZE: f32 = 14.0;
const SELECTION_BG: Color32 = Color32::from_rgb(38, 79, 120);
const SEARCH_BG: Color32 = Color32::from_rgb(110, 90, 20);
const DIFF_BG: Color32 = Color32::from_rgb(120, 28, 28);
const CURSOR_OUTLINE: Color32 = Color32::from_rgb(255, 210, 80);
const CURSOR_OUTLINE_DIM: Color32 = Color32::from_rgb(150, 150, 150);
const OFFSET_FG: Color32 = Color32::from_rgb(120, 130, 150);
const HEADER_FG: Color32 = Color32::from_rgb(140, 150, 170);

/// Horizontal geometry shared by the header strip and every byte row.
#[derive(Clone, Copy)]
struct Layout {
    left: f32,
    char_w: f32,
    byte_step: f32,
    group_gap: f32,
    ascii_gap: f32,
    off_w: f32,
    bpr: usize,
    show_ascii: bool,
}

impl Layout {
    fn hex_x(&self, i: usize) -> f32 {
        self.left
            + self.off_w
            + (i as f32) * self.byte_step * self.char_w
            + ((i / 8) as f32) * self.group_gap * self.char_w
    }
    fn hex_left(&self) -> f32 {
        self.hex_x(0)
    }
    fn hex_end(&self) -> f32 {
        self.hex_x(self.bpr - 1) + 2.0 * self.char_w
    }
    fn ascii_left(&self) -> f32 {
        self.hex_end() + self.ascii_gap * self.char_w
    }
    fn ascii_x(&self, i: usize) -> f32 {
        self.ascii_left() + i as f32 * self.char_w
    }
    fn width(&self) -> f32 {
        let right = if self.show_ascii {
            self.ascii_x(self.bpr)
        } else {
            self.hex_end()
        };
        right - self.left + self.char_w
    }

    /// Exact hit-test: the byte column under `x` and the area it is in.
    fn hit_test(&self, x: f32) -> Option<(usize, EditColumn)> {
        let half_gap = ((self.byte_step - 2.0).max(0.2) * 0.5) * self.char_w;
        for i in 0..self.bpr {
            let hx = self.hex_x(i);
            if x >= hx - half_gap && x < hx + 2.0 * self.char_w + half_gap {
                return Some((i, EditColumn::Hex));
            }
        }
        if self.show_ascii {
            let al = self.ascii_left();
            if x >= al && x < self.ascii_x(self.bpr) {
                let i = ((x - al) / self.char_w).floor() as usize;
                return Some((i.min(self.bpr - 1), EditColumn::Ascii));
            }
        }
        None
    }

    /// Like `hit_test`, but snaps to the nearest edge column (used while dragging).
    fn hit_test_clamped(&self, x: f32, fallback: EditColumn) -> (usize, EditColumn) {
        self.hit_test(x).unwrap_or_else(|| {
            if x < self.hex_left() {
                (0, fallback)
            } else {
                (self.bpr - 1, fallback)
            }
        })
    }
}

fn in_search_match(matches: &[SearchMatch], abs: usize) -> bool {
    let idx = matches.partition_point(|m| m.start <= abs);
    idx > 0 && matches[idx - 1].contains(abs)
}

fn ascii_char(byte: u8) -> char {
    if (0x20..0x7f).contains(&byte) {
        byte as char
    } else {
        '.'
    }
}

pub fn render_hex_viewport(
    ui: &mut egui::Ui,
    viewport_id: &str,
    doc: &mut Document,
    search_matches: &[SearchMatch],
    diff_other: Option<&ByteBuffer>,
    active_column: &mut EditColumn,
    request_add_field: &mut bool,
) {
    // Strictly clip this viewport to its allocated region so side-by-side panes never overlap.
    let pane_clip = ui.max_rect().intersect(ui.clip_rect());
    ui.set_clip_rect(pane_clip);

    let bpr = doc.settings.bytes_per_row.clamp(1, 64);
    let show_ascii = doc.settings.show_ascii;
    let uppercase = doc.settings.uppercase_hex;

    let avail_w = ui.available_width();
    let base_font = FontId::monospace(FONT_SIZE);
    let base_char_w = ui.fonts(|f| f.glyph_width(&base_font, '0')).max(7.5);

    let spacious_chars = (if doc.settings.show_offsets { 10.0 } else { 0.0 })
        + ((bpr - 1) * 3 + (bpr - 1) / 8 + 2) as f32
        + (if show_ascii { 2.0 + bpr as f32 } else { 0.0 })
        + 1.0;
    let needs_compact = avail_w < spacious_chars * base_char_w + 16.0;
    let (off_chars, byte_step, group_gap, ascii_gap) = if needs_compact {
        (
            if doc.settings.show_offsets { 9.0 } else { 0.0 },
            2.6,
            0.5,
            1.2,
        )
    } else {
        (
            if doc.settings.show_offsets { 10.0 } else { 0.0 },
            3.0,
            1.0,
            2.0,
        )
    };
    let total_chars = off_chars
        + (bpr - 1) as f32 * byte_step
        + ((bpr - 1) / 8) as f32 * group_gap
        + 2.0
        + (if show_ascii { ascii_gap + bpr as f32 } else { 0.0 })
        + 1.0;

    let usable_w = (avail_w - 14.0).max(120.0);
    let font_size = if usable_w < total_chars * base_char_w {
        let scale = (usable_w / (total_chars * base_char_w)).clamp(0.70, 1.0);
        (FONT_SIZE * scale).clamp(10.0, FONT_SIZE)
    } else {
        FONT_SIZE
    };

    let font = FontId::monospace(font_size);
    let char_w = ui.fonts(|f| f.glyph_width(&font, '0')).max(5.5);
    let row_height = (ui.fonts(|f| f.row_height(&font)) + 2.0).max(16.0);
    let off_w = off_chars * char_w;

    // ---- Header strip: mode badge + selection summary ----
    ui.horizontal(|ui| {
        ui.set_max_width(avail_w);
        if diff_other.is_some() || viewport_id != "hex_viewport_primary" {
            ui.strong(doc.file_name());
            ui.separator();
        }
        let (badge, color) = if !doc.edit_mode {
            ("VIEW", Color32::from_rgb(70, 110, 160))
        } else if matches!(doc.edit_kind, EditKind::Insert) {
            ("INSERT", Color32::from_rgb(170, 90, 40))
        } else {
            ("OVERWRITE", Color32::from_rgb(160, 50, 50))
        };
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(format!(" {badge} "))
                        .monospace()
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(color),
            )
            .on_hover_text("Click or press 'i' to cycle View / Overwrite / Insert mode")
            .clicked()
        {
            doc.cycle_edit_mode();
        }
        let col = match active_column {
            EditColumn::Hex => "HEX",
            EditColumn::Ascii => "ASCII",
        };
        if ui
            .selectable_label(true, egui::RichText::new(col).monospace().color(HEADER_FG))
            .on_hover_text("Click to switch active editing column (Hex vs ASCII)")
            .clicked()
        {
            *active_column = match active_column {
                EditColumn::Hex => EditColumn::Ascii,
                EditColumn::Ascii => EditColumn::Hex,
            };
        }
        let summary = match &doc.selection {
            Some(sel) if sel.anchor != sel.cursor => {
                let (a, b) = (sel.start(), sel.end());
                format!("sel 0x{a:08X}..0x{b:08X} ({} bytes)", sel.len())
            }
            Some(sel) => format!("cursor 0x{:08X} ({})", sel.cursor, sel.cursor),
            None => "empty buffer".to_owned(),
        };
        let extra = if doc.additional_selections.is_empty() {
            String::new()
        } else {
            format!("  +{} selections", doc.additional_selections.len())
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(summary + &extra)
                    .monospace()
                    .color(HEADER_FG),
            )
            .truncate(),
        );
        if !doc.status.is_empty() {
            ui.separator();
            ui.add(egui::Label::new(egui::RichText::new(&doc.status).monospace().weak()).truncate());
        }
    });

    // ---- Header strip: column offsets ----
    let (hrect, _) =
        ui.allocate_exact_size(Vec2::new(avail_w, row_height), Sense::hover());
    let header_layout = Layout {
        left: hrect.left(),
        char_w,
        byte_step,
        group_gap,
        ascii_gap,
        off_w,
        bpr,
        show_ascii,
    };
    {
        let painter = ui.painter().with_clip_rect(pane_clip);
        let y = hrect.top() + 1.0;
        if doc.settings.show_offsets {
            painter.text(
                Pos2::new(hrect.left(), y),
                Align2::LEFT_TOP,
                "Offset",
                font.clone(),
                HEADER_FG,
            );
        }
        for i in 0..bpr {
            let label = hex_byte_str(i as u8, uppercase);
            painter.text(
                Pos2::new(header_layout.hex_x(i), y),
                Align2::LEFT_TOP,
                label,
                font.clone(),
                HEADER_FG,
            );
        }
        if show_ascii {
            painter.text(
                Pos2::new(header_layout.ascii_left(), y),
                Align2::LEFT_TOP,
                "ASCII",
                font.clone(),
                HEADER_FG,
            );
        }
    }
    ui.separator();

    ui.spacing_mut().item_spacing.y = 0.0;
    let total_rows = doc.row_count();

    let mut scroll_area = if header_layout.width() > avail_w + 2.0 {
        egui::ScrollArea::both()
    } else {
        egui::ScrollArea::vertical()
    }
    .id_salt(viewport_id)
    .max_width(avail_w)
    .auto_shrink([false, false])
    .animated(false)
    .drag_to_scroll(false);
    if std::mem::take(&mut doc.scroll_requested) {
        scroll_area = scroll_area.vertical_scroll_offset(doc.scroll as f32 * row_height);
    }

    let pending_insert = doc.pending_insert_nibble();

    scroll_area.show_rows(ui, row_height, total_rows, |ui, row_range| {
            let row_clip = ui.clip_rect().intersect(pane_clip);
            ui.set_clip_rect(row_clip);
            doc.visible_rows = row_range.len().max(1);
            doc.scroll = row_range.start;
            let buf_len = doc.buffer.len();

            for row in row_range {
                match doc.display_row(row) {
                    DisplayRow::Repeated {
                        start,
                        end,
                        byte,
                        physical_rows,
                    } => {
                        let width = avail_w.max(header_layout.width());
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(width, row_height), Sense::click());
                        let painter = ui.painter().with_clip_rect(row_clip);
                        if resp.hovered() {
                            painter.rect_filled(rect, 0.0, Color32::from_white_alpha(10));
                        }
                        let text = format!(
                            "{start:08X}  ... {physical_rows} identical rows of 0x{byte:02X} compressed (through {end:08X}) ..."
                        );
                        painter.text(
                            Pos2::new(rect.left(), rect.top() + 1.0),
                            Align2::LEFT_TOP,
                            text,
                            font.clone(),
                            OFFSET_FG,
                        );
                        if resp.clicked() {
                            if let Some(focused) = ui.ctx().memory(|m| m.focused()) {
                                ui.ctx().memory_mut(|m| m.surrender_focus(focused));
                            }
                            doc.select_offset(start, ui.input(|i| i.modifiers.shift));
                        }
                    }
                    DisplayRow::Bytes { offset } => {
                        let mut row_buf = [0u8; 64];
                        let count = doc.buffer.copy_range(offset, &mut row_buf[..bpr]);

                        let width = header_layout.width().min(avail_w.max(header_layout.width()));
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(width, row_height),
                            Sense::click_and_drag(),
                        );
                        let lay = Layout {
                            left: rect.left(),
                            ..header_layout
                        };
                        let painter = ui.painter().with_clip_rect(row_clip);
                        let ty = rect.top() + 1.0;

                        if doc.settings.show_offsets {
                            let off_str = if uppercase {
                                format!("{offset:08X}")
                            } else {
                                format!("{offset:08x}")
                            };
                            painter.text(
                                Pos2::new(rect.left(), ty),
                                Align2::LEFT_TOP,
                                off_str,
                                font.clone(),
                                OFFSET_FG,
                            );
                        }

                        let cursor = doc.selection.as_ref().map(|s| s.cursor);
                        for i in 0..count {
                            let abs = offset + i;
                            let byte = row_buf[i];
                            let selected = doc.is_selected(abs);
                            let field = if doc.settings.show_overlays {
                                doc.fields.iter().find(|f| f.contains(abs))
                            } else {
                                None
                            };

                            let bg = if diff_other.is_some_and(|o| o.get(abs) != Some(byte)) {
                                Some(DIFF_BG)
                            } else if selected {
                                Some(SELECTION_BG)
                            } else if in_search_match(search_matches, abs) {
                                Some(SEARCH_BG)
                            } else if let Some(f) = field {
                                f.background
                                    .map(|bg_col| field_color32(bg_col).gamma_multiply(0.35))
                                    .or_else(|| Some(field_color32(f.color).gamma_multiply(0.14)))
                            } else {
                                None
                            };
                            let fg = if selected {
                                Color32::WHITE
                            } else if doc.buffer.is_modified(abs) {
                                named_color32(doc.theme.modified)
                            } else if let Some(f) = field {
                                field_color32(f.color)
                            } else {
                                byte_color32(&doc.theme, abs, byte, false)
                            };

                            let hex_rect = Rect::from_min_size(
                                Pos2::new(lay.hex_x(i) - 0.5 * char_w, rect.top()),
                                Vec2::new(3.0 * char_w, row_height),
                            );
                            let ascii_rect = Rect::from_min_size(
                                Pos2::new(lay.ascii_x(i), rect.top()),
                                Vec2::new(char_w, row_height),
                            );

                            if let Some(bg) = bg {
                                painter.rect_filled(hex_rect, 0.0, bg);
                                if show_ascii {
                                    painter.rect_filled(ascii_rect, 0.0, bg);
                                }
                            }
                            if let Some(f) = field {
                                let stroke = Stroke::new(1.5_f32, field_color32(f.color));
                                let y = rect.bottom() - 1.0;
                                painter.hline(hex_rect.x_range(), y, stroke);
                                if abs == f.start {
                                    painter.vline(hex_rect.left() + 1.0, rect.y_range(), stroke);
                                }
                                if abs == f.end {
                                    painter.vline(hex_rect.right() - 1.0, rect.y_range(), stroke);
                                }
                                if show_ascii {
                                    painter.hline(ascii_rect.x_range(), y, stroke);
                                }
                            }
                            if cursor == Some(abs) {
                                let (h, a) = match active_column {
                                    EditColumn::Hex => (CURSOR_OUTLINE, CURSOR_OUTLINE_DIM),
                                    EditColumn::Ascii => (CURSOR_OUTLINE_DIM, CURSOR_OUTLINE),
                                };
                                painter.rect_stroke(
                                    hex_rect,
                                    0.0,
                                    Stroke::new(1.2_f32, h),
                                    egui::StrokeKind::Inside,
                                );
                                if show_ascii {
                                    painter.rect_stroke(
                                        ascii_rect,
                                        0.0,
                                        Stroke::new(1.2_f32, a),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                            }

                            let pending_here = pending_insert.filter(|(poff, _)| *poff == abs);
                            let hex_text: String;
                            let hex_label: &str = if let Some((_, hi)) = pending_here {
                                hex_text = if uppercase {
                                    format!("{hi:X}_")
                                } else {
                                    format!("{hi:x}_")
                                };
                                &hex_text
                            } else {
                                hex_byte_str(byte, uppercase)
                            };
                            let draw_fg = if pending_here.is_some() {
                                CURSOR_OUTLINE
                            } else {
                                fg
                            };

                            painter.text(
                                Pos2::new(lay.hex_x(i), ty),
                                Align2::LEFT_TOP,
                                hex_label,
                                font.clone(),
                                draw_fg,
                            );
                            if show_ascii {
                                let afg =
                                    if selected || doc.buffer.is_modified(abs) || field.is_some() {
                                        fg
                                    } else {
                                        byte_color32(&doc.theme, abs, byte, true)
                                    };
                                let mut ch_buf = [0u8; 4];
                                let ch_str = ascii_char(byte).encode_utf8(&mut ch_buf);
                                painter.text(
                                    Pos2::new(lay.ascii_x(i), ty),
                                    Align2::LEFT_TOP,
                                    ch_str,
                                    font.clone(),
                                    afg,
                                );
                            }
                        }

                        // ---- Hover tooltip for overlay fields ----
                        if resp.hovered()
                            && doc.settings.show_overlays
                            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                            && let Some((col, _)) = lay.hit_test(pos.x)
                            && col < count
                        {
                            let hovered_off = offset + col;
                            if let Some(f) = doc.fields.iter().find(|f| f.contains(hovered_off)) {
                                let tip = if f.description.is_empty() {
                                    format!(
                                        "Field: {}\nRange: 0x{:X}..0x{:X} ({} bytes)",
                                        f.name,
                                        f.start,
                                        f.end,
                                        f.len()
                                    )
                                } else {
                                    format!(
                                        "Field: {}\nRange: 0x{:X}..0x{:X} ({} bytes)\n{}",
                                        f.name,
                                        f.start,
                                        f.end,
                                        f.len(),
                                        f.description
                                    )
                                };
                                resp.clone().on_hover_text(tip);
                            }
                        }

                        // ---- Mouse interaction ----
                        let pointer = ui.input(|i| i.pointer.interact_pos());
                        let (ctrl, shift) =
                            ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
                        if resp.clicked() || resp.drag_started() {
                            if let Some(focused) = ui.ctx().memory(|m| m.focused()) {
                                ui.ctx().memory_mut(|m| m.surrender_focus(focused));
                            }
                            if let Some(pos) = pointer
                                && let Some((col, area)) = lay.hit_test(pos.x)
                                && col < count
                            {
                                let clicked = offset + col;
                                if doc.edit_mode {
                                    doc.commit_pending_edit();
                                }
                                *active_column = area;
                                if ctrl {
                                    doc.add_selection(clicked);
                                } else {
                                    doc.select_offset(clicked, shift);
                                }
                            }
                        } else if resp.dragged()
                            && let Some(pos) = pointer
                        {
                            let (col, _) = lay.hit_test_clamped(pos.x, *active_column);
                            let row_delta = ((pos.y - rect.top()) / row_height).floor() as isize;
                            let target =
                                offset as isize + row_delta * bpr as isize + col as isize;
                            let max = buf_len.saturating_sub(1) as isize;
                            let dragged = target.clamp(0, max) as usize;
                            if let Some(sel) = &mut doc.selection {
                                sel.cursor = dragged;
                            }
                        }

                        resp.context_menu(|ui| {
                            ui.set_min_width(190.0);
                            for (label, fmt) in [
                                ("Copy Hex (Ctrl+C)", CopyFormat::HexSpaced),
                                ("Copy Compact Hex", CopyFormat::HexCompact),
                                ("Copy ASCII (Ctrl+Shift+C)", CopyFormat::Ascii),
                                ("Copy as C Array", CopyFormat::CArray),
                                ("Copy as Rust Array", CopyFormat::RustArray),
                                ("Copy as Python Bytes", CopyFormat::PythonBytes),
                            ] {
                                if ui.button(label).clicked() {
                                    if let Some(text) = doc.format_selected_bytes(fmt) {
                                        ui.ctx().copy_text(text.clone());
                                        if let Ok(mut cb) = arboard::Clipboard::new() {
                                            let _ = cb.set_text(text);
                                        }
                                        doc.status = format!("Copied selection ({label})");
                                    }
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button("Paste Hex (Ctrl+V)").clicked() {
                                if let Ok(mut cb) = arboard::Clipboard::new()
                                    && let Ok(text) = cb.get_text()
                                {
                                    doc.paste_hex_bytes(&text);
                                }
                                ui.close_menu();
                            }
                            if ui.button("Paste ASCII (Ctrl+Shift+V)").clicked() {
                                if let Ok(mut cb) = arboard::Clipboard::new()
                                    && let Ok(text) = cb.get_text()
                                {
                                    doc.paste_ascii_bytes(&text);
                                }
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("+ Add Overlay Field for Selection (a)").clicked() {
                                *request_add_field = true;
                                ui.close_menu();
                            }
                            if ui.button("Delete Selected Bytes (Del)").clicked() {
                                doc.delete_selected_bytes();
                                ui.close_menu();
                            }
                        });
                    }
                }
            }
        });
}
