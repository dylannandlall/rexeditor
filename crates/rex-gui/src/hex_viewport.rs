//! Virtualized hex viewport for `rex-gui`.
//!
//! Each row is painted in a single pass with `ui.painter()`, rather than allocating
//! separate widgets per byte. This keeps multi-gigabyte files fast to scroll and lets
//! a single hit-test map pointer positions to either the Hex or ASCII column.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use rex_core::{ByteBuffer, CopyFormat, DisplayRow, Document, EditKind, Field, SearchMatch};

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

/// Which sides of a byte cell lie on the outside of its field's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BorderEdges {
    top: bool,
    bottom: bool,
    left: bool,
    right: bool,
}

impl BorderEdges {
    /// Edges of the byte at `abs` (column `i` of a row holding `count` bytes, `bpr`
    /// per row) that face outside `field`, i.e. where its border must be drawn.
    fn for_byte(field: &Field, abs: usize, i: usize, count: usize, bpr: usize) -> Self {
        Self {
            top: abs < bpr || !field.contains(abs - bpr),
            bottom: !field.contains(abs + bpr),
            left: i == 0 || !field.contains(abs - 1),
            right: i + 1 == count || !field.contains(abs + 1),
        }
    }
}

fn paint_border(painter: &egui::Painter, cell: Rect, edges: BorderEdges, stroke: Stroke) {
    let inset = stroke.width * 0.5;
    let (l, r) = (cell.left() + inset, cell.right() - inset);
    let (t, b) = (cell.top() + inset, cell.bottom() - inset);
    if edges.top {
        painter.hline(cell.left()..=cell.right(), t, stroke);
    }
    if edges.bottom {
        painter.hline(cell.left()..=cell.right(), b, stroke);
    }
    if edges.left {
        painter.vline(l, cell.y_range(), stroke);
    }
    if edges.right {
        painter.vline(r, cell.y_range(), stroke);
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
    // Set while a drag-selection started in this viewport is in progress. The drag is
    // tracked at the viewport level (not per row) so it survives the starting row
    // scrolling out of view during edge auto-scroll.
    let drag_id = egui::Id::new((viewport_id, "drag_select"));

    let output = scroll_area.show_rows(ui, row_height, total_rows, |ui, row_range| {
            let row_clip = ui.clip_rect().intersect(pane_clip);
            ui.set_clip_rect(row_clip);
            doc.visible_rows = row_range.len().max(1);
            doc.scroll = row_range.start;

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
                                doc.fields.iter().find(|f| f.visible && f.contains(abs))
                            } else {
                                None
                            };
                            let field_text = field.and_then(|f| f.text);

                            let bg = if diff_other.is_some_and(|o| o.get(abs) != Some(byte)) {
                                Some(DIFF_BG)
                            } else if selected {
                                Some(SELECTION_BG)
                            } else if in_search_match(search_matches, abs) {
                                Some(SEARCH_BG)
                            } else {
                                field
                                    .and_then(|f| f.background)
                                    .map(|c| field_color32(c).gamma_multiply(0.45))
                            };
                            let fg = if selected {
                                Color32::WHITE
                            } else if doc.buffer.is_modified(abs) {
                                named_color32(doc.theme.modified)
                            } else if let Some(c) = field_text {
                                field_color32(c)
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
                            if let Some(f) = field
                                && let Some(border) = f.border
                            {
                                let stroke = Stroke::new(1.5_f32, field_color32(border));
                                let edges = BorderEdges::for_byte(f, abs, i, count, bpr);
                                // Extend top/bottom lines across the gap to the next cell
                                // so a multi-byte border reads as one continuous box.
                                let hex_span_right = if edges.right {
                                    hex_rect.right()
                                } else {
                                    lay.hex_x(i + 1) - 0.5 * char_w
                                };
                                let hex_border = Rect::from_x_y_ranges(
                                    hex_rect.left()..=hex_span_right,
                                    rect.y_range(),
                                );
                                paint_border(&painter, hex_border, edges, stroke);
                                if show_ascii {
                                    paint_border(&painter, ascii_rect, edges, stroke);
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
                                let afg = if selected
                                    || doc.buffer.is_modified(abs)
                                    || field_text.is_some()
                                {
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
                            if let Some(f) = doc
                                .fields
                                .iter()
                                .find(|f| f.visible && f.contains(hovered_off))
                            {
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
                        // A drag is only recognized after the pointer moves a few pixels, so
                        // anchor it where the button went down rather than where it is now.
                        let pointer = ui.input(|i| {
                            if resp.drag_started() {
                                i.pointer.press_origin().or(i.pointer.interact_pos())
                            } else {
                                i.pointer.interact_pos()
                            }
                        });
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
                                if resp.drag_started() && !ctrl {
                                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, true));
                                }
                            }
                        }

                        resp.context_menu(|ui| {
                            ui.set_min_width(190.0);
                            for (label, fmt) in [
                                ("Copy Hex (Ctrl+C)", CopyFormat::HexCompact),
                                ("Copy Hex with Spaces (Ctrl+Alt+C)", CopyFormat::HexSpaced),
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

    update_drag_selection(
        ui,
        doc,
        &output,
        drag_id,
        Layout {
            left: output.inner_rect.left() - output.state.offset.x,
            ..header_layout
        },
        row_height,
        total_rows,
        *active_column,
    );
}

/// Extends an in-progress drag selection to the byte under the pointer and auto-scrolls
/// the viewport when the pointer is held near (or beyond) its top or bottom edge.
#[allow(clippy::too_many_arguments)]
fn update_drag_selection(
    ui: &egui::Ui,
    doc: &mut Document,
    output: &egui::scroll_area::ScrollAreaOutput<()>,
    drag_id: egui::Id,
    lay: Layout,
    row_height: f32,
    total_rows: usize,
    active_column: EditColumn,
) {
    let ctx = ui.ctx();
    if !ctx.data(|d| d.get_temp::<bool>(drag_id)).unwrap_or(false) {
        return;
    }
    let (primary_down, pointer, dt) =
        ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos(), i.stable_dt));
    if !primary_down {
        ctx.data_mut(|d| d.remove::<bool>(drag_id));
        return;
    }
    let Some(pos) = pointer else {
        return;
    };
    if total_rows == 0 || doc.buffer.is_empty() {
        return;
    }

    let inner = output.inner_rect;
    let mut state = output.state;

    // Edge auto-scroll: speed grows with how far into (or past) the edge zone the pointer is.
    let edge = (row_height * 1.5).min(inner.height() * 0.25).max(1.0);
    let depth = if pos.y < inner.top() + edge {
        pos.y - (inner.top() + edge)
    } else if pos.y > inner.bottom() - edge {
        pos.y - (inner.bottom() - edge)
    } else {
        0.0
    };
    let mut lay = lay;
    if depth != 0.0 {
        let rows_per_sec = (depth / edge * 10.0).clamp(-80.0, 80.0);
        let delta = rows_per_sec * row_height * dt.clamp(0.0, 0.1);
        let max_offset = (output.content_size.y - inner.height()).max(0.0);
        let new_offset = (state.offset.y + delta).clamp(0.0, max_offset);
        if new_offset != state.offset.y {
            state.offset.y = new_offset;
            state.store(ctx, output.id);
        }
        // Keep frames coming while the pointer rests in the edge zone.
        ctx.request_repaint();
    }
    lay.left = inner.left() - state.offset.x;

    let y = pos.y.clamp(inner.top(), (inner.bottom() - 1.0).max(inner.top()));
    let row = (((y - inner.top() + state.offset.y) / row_height).floor().max(0.0) as usize)
        .min(total_rows - 1);
    let (col, _) = lay.hit_test_clamped(pos.x, active_column);
    let target = match doc.display_row(row) {
        DisplayRow::Bytes { offset } => offset + col,
        DisplayRow::Repeated { start, .. } => start,
    };
    let target = target.min(doc.buffer.len() - 1);
    if let Some(sel) = &mut doc.selection
        && sel.cursor != target
    {
        sel.cursor = target;
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Harness, find_text};
    use egui::{FullOutput, Shape};
    use rex_core::FieldColor;

    fn field(start: usize, end: usize) -> Field {
        Field {
            name: "f".into(),
            start,
            end,
            ..Default::default()
        }
    }

    #[test]
    fn border_edges_outline_a_multi_row_field() {
        let f = field(4, 20);
        let edges = |abs: usize, count: usize| BorderEdges::for_byte(&f, abs, abs % 16, count, 16);
        // First byte: top and left edges only.
        assert_eq!(
            edges(4, 16),
            BorderEdges { top: true, bottom: false, left: true, right: false }
        );
        // End of the first row: the row break closes the right side.
        assert_eq!(
            edges(15, 16),
            BorderEdges { top: true, bottom: true, left: false, right: true }
        );
        // Start of the second row opens on the left.
        assert_eq!(
            edges(16, 16),
            BorderEdges { top: true, bottom: true, left: true, right: false }
        );
        // Last byte: below it and to its right are outside the field.
        assert_eq!(
            edges(20, 16),
            BorderEdges { top: false, bottom: true, left: false, right: true }
        );
        // Last byte of a short final row closes on the right.
        assert!(BorderEdges::for_byte(&field(0, 100), 17, 1, 2, 16).right);
    }

    fn draw(h: &mut Harness, doc: &mut Document, column: &mut EditColumn) -> FullOutput {
        let mut add = false;
        h.frame(|ui| render_hex_viewport(ui, "hex_viewport_primary", doc, &[], None, column, &mut add))
    }

    /// Stroke, fill and text colors used in a frame.
    fn colors(out: &FullOutput) -> (Vec<Color32>, Vec<Color32>, Vec<Color32>) {
        let (mut lines, mut fills, mut texts) = (Vec::new(), Vec::new(), Vec::new());
        for clipped in &out.shapes {
            match &clipped.shape {
                Shape::LineSegment { stroke, .. } => lines.push(stroke.color),
                Shape::Rect(r) => fills.push(r.fill),
                Shape::Text(t) => texts.extend(t.galley.job.sections.iter().map(|s| s.format.color)),
                _ => {}
            }
        }
        (lines, fills, texts)
    }

    #[test]
    fn field_border_text_and_background_are_drawn_only_while_visible() {
        let mut doc = Document::from_bytes("t.bin".into(), vec![0x41; 64]);
        doc.select_offset(40, false);
        doc.add_field(Field {
            border: Some(FieldColor::Red),
            text: Some(FieldColor::Green),
            background: Some(FieldColor::Blue),
            ..field(2, 5)
        });
        let mut column = EditColumn::Hex;
        let mut h = Harness::new(900.0, 400.0);
        draw(&mut h, &mut doc, &mut column);
        let (lines, fills, texts) = colors(&draw(&mut h, &mut doc, &mut column));
        let red = field_color32(FieldColor::Red);
        let green = field_color32(FieldColor::Green);
        let blue_bg = field_color32(FieldColor::Blue).gamma_multiply(0.45);
        assert!(lines.contains(&red), "border should be drawn");
        assert!(texts.contains(&green), "text color should be used");
        assert!(fills.contains(&blue_bg), "background should be filled");

        doc.fields[0].visible = false;
        let (lines, fills, texts) = colors(&draw(&mut h, &mut doc, &mut column));
        assert!(!lines.contains(&red));
        assert!(!texts.contains(&green));
        assert!(!fills.contains(&blue_bg));
    }

    #[test]
    fn border_only_field_leaves_text_and_background_alone() {
        let mut doc = Document::from_bytes("t.bin".into(), vec![0x41; 64]);
        doc.add_field(Field {
            border: Some(FieldColor::Magenta),
            ..field(2, 5)
        });
        let mut column = EditColumn::Hex;
        let mut h = Harness::new(900.0, 400.0);
        draw(&mut h, &mut doc, &mut column);
        let (lines, fills, texts) = colors(&draw(&mut h, &mut doc, &mut column));
        let magenta = field_color32(FieldColor::Magenta);
        assert!(lines.contains(&magenta));
        assert!(!texts.contains(&magenta));
        assert!(!fills.iter().any(|c| *c == magenta.gamma_multiply(0.45)));
    }

    /// Position of byte `col` on the row whose offset label reads `label`.
    fn byte_pos(out: &FullOutput, label: &str, col: usize) -> Pos2 {
        let r = find_text(out, label).unwrap_or_else(|| panic!("row {label} not drawn"));
        let char_w = r.width() / label.len() as f32;
        // Offsets take 10 columns; each byte 3; +1 for centre of the first digit.
        Pos2::new(r.left() + (10 + 3 * col) as f32 * char_w + char_w, r.center().y)
    }

    #[test]
    fn dragging_past_the_bottom_edge_scrolls_and_extends_selection() {
        let mut doc = Document::from_bytes("t.bin".into(), (0..16 * 400).map(|i| i as u8).collect());
        doc.select_offset(0, false);
        let mut column = EditColumn::Hex;
        let mut h = Harness::new(900.0, 420.0);
        draw(&mut h, &mut doc, &mut column);
        let out = draw(&mut h, &mut doc, &mut column);
        assert_eq!(doc.scroll, 0);

        let start = byte_pos(&out, "00000030", 2);
        h.press(start);
        draw(&mut h, &mut doc, &mut column);
        // The first move crosses into the next byte before egui recognises the drag.
        for delta in [Vec2::new(30.0, 0.0), Vec2::new(30.0, 40.0), Vec2::new(0.0, 120.0)] {
            h.move_to(start + delta);
            draw(&mut h, &mut doc, &mut column);
        }
        assert_eq!(doc.selection.unwrap().anchor, 0x32, "drag anchors where it was pressed");
        assert_eq!(doc.scroll, 0, "no scrolling away from the edges");

        // Hold the pointer below the viewport.
        h.move_to(Pos2::new(start.x, 418.0));
        for _ in 0..40 {
            draw(&mut h, &mut doc, &mut column);
        }
        let scrolled = doc.scroll;
        assert!(scrolled > 5, "viewport should auto-scroll down, scroll = {scrolled}");
        let sel = doc.selection.unwrap();
        assert_eq!(sel.anchor, 0x32);
        assert!(sel.cursor / 16 >= scrolled, "selection follows the scrolled rows");

        // Releasing stops the auto-scroll.
        h.release(Pos2::new(start.x, 418.0));
        draw(&mut h, &mut doc, &mut column);
        let after_release = doc.scroll;
        for _ in 0..10 {
            draw(&mut h, &mut doc, &mut column);
        }
        assert_eq!(doc.scroll, after_release);
    }

    #[test]
    fn dragging_past_the_top_edge_scrolls_up() {
        let mut doc = Document::from_bytes("t.bin".into(), (0..16 * 400).map(|i| i as u8).collect());
        doc.select_offset(16 * 200, false);
        let mut column = EditColumn::Hex;
        let mut h = Harness::new(900.0, 420.0);
        draw(&mut h, &mut doc, &mut column);
        let out = draw(&mut h, &mut doc, &mut column);
        let top_row = doc.scroll;
        assert!(top_row > 100);

        let label = format!("{:08X}", (top_row + 6) * 16);
        let start = byte_pos(&out, &label, 0);
        h.press(start);
        draw(&mut h, &mut doc, &mut column);
        h.move_to(start - Vec2::new(0.0, 20.0));
        draw(&mut h, &mut doc, &mut column);
        h.move_to(Pos2::new(start.x, 2.0));
        for _ in 0..40 {
            draw(&mut h, &mut doc, &mut column);
        }
        assert!(doc.scroll + 5 < top_row, "viewport should auto-scroll up");
        let sel = doc.selection.unwrap();
        assert!(sel.cursor < sel.anchor);
        h.release(Pos2::new(start.x, 2.0));
        draw(&mut h, &mut doc, &mut column);
    }
}
