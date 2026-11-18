use std::collections::HashMap;

use egui::{Color32, Sense, Stroke, Vec2};
use rex_core::entropy::EntropyReport;
use rex_core::plugins::{ERROR_PLACEHOLDER, NOT_RUN_PLACEHOLDER, PENDING_PLACEHOLDER, Plugin};
use rex_core::{Document, Endianness, Field, FieldColor, InspectorData, InspectorValue};

use crate::theme::field_color32;

#[derive(Default)]
pub struct InspectorUiState {
    pub endianness: Endianness,
    pub editing_kind: Option<String>,
    pub edit_buffer: String,
    /// Set for one frame when an edit starts so the text box grabs keyboard focus.
    pub focus_edit: bool,
}

#[derive(Default)]
pub struct FieldEditorState {
    pub open: bool,
    pub editing_index: Option<usize>,
    pub name: String,
    pub description: String,
    pub start_str: String,
    pub end_str: String,
    pub border: Option<FieldColor>,
    pub text: Option<FieldColor>,
    pub background: Option<FieldColor>,
    pub visible: bool,
    pub filter: String,
}

impl FieldEditorState {
    pub fn begin_add_for_selection(&mut self, doc: &Document) {
        let (s, e) = doc
            .selection
            .as_ref()
            .map(|sel| (sel.start(), sel.end()))
            .unwrap_or((0, 0));
        let filter = std::mem::take(&mut self.filter);
        *self = Self {
            open: true,
            name: format!("field_{}", doc.fields.len() + 1),
            start_str: format!("0x{s:X}"),
            end_str: format!("0x{e:X}"),
            // Only a border by default; rotate colors so adjacent fields differ.
            border: Some(FieldColor::ALL[doc.fields.len() % FieldColor::ALL.len()]),
            visible: true,
            filter,
            ..Default::default()
        };
    }
}

fn truncate_display(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Renders one inspector row and returns the value widget's response.
///
/// Clicking the value copies it to the clipboard. When `kind` is `Some`, double-clicking
/// turns the value into a text box: Enter writes the new value, Esc or clicking away
/// cancels.
fn value_row(
    ui: &mut egui::Ui,
    doc: &mut Document,
    state: &mut InspectorUiState,
    range_start: usize,
    label: &str,
    value: &str,
    kind: Option<&str>,
) -> egui::Response {
    ui.label(label);
    let editing = kind.is_some() && state.editing_kind.as_deref() == kind;
    let resp = if editing {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.edit_buffer)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width().max(80.0)),
        );
        if std::mem::take(&mut state.focus_edit) {
            resp.request_focus();
        }
        if resp.lost_focus() {
            let (enter, escape) =
                ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter
                && !escape
                && let Some(k) = kind
            {
                match InspectorValue::parse_as(k, &state.edit_buffer, state.endianness) {
                    Ok(bytes) => {
                        doc.overwrite_bytes(range_start, &bytes);
                        doc.status = format!("Wrote {label} = {}", state.edit_buffer.trim());
                    }
                    Err(err) => doc.status = err,
                }
            }
            state.editing_kind = None;
        }
        resp
    } else {
        let copyable = value != "-" && !value.is_empty();
        let display = truncate_display(value, 22);
        let resp = copyable_value(ui, &display);
        let hint = match (copyable, kind.is_some()) {
            (true, true) => "Click to copy · Double-click to edit",
            (true, false) => "Click to copy",
            (false, true) => "Double-click to edit",
            (false, false) => "",
        };
        let resp = if display != value {
            resp.on_hover_text(format!("{value}\n\n{hint}"))
        } else if !hint.is_empty() {
            resp.on_hover_text(hint)
        } else {
            resp
        };
        if let Some(k) = kind
            && resp.double_clicked()
        {
            state.editing_kind = Some(k.to_string());
            state.edit_buffer = if copyable { value.to_string() } else { String::new() };
            state.focus_edit = true;
        } else if copyable && resp.clicked() {
            copy_to_clipboard(ui, value);
            doc.status = format!("Copied {label}: {display}");
        }
        resp
    };
    ui.end_row();
    resp
}

/// A monospace value that behaves like a read-only text box: it highlights on hover
/// and reports clicks.
fn copyable_value(ui: &mut egui::Ui, display: &str) -> egui::Response {
    let resp = ui.add(
        egui::Label::new(egui::RichText::new(display).monospace())
            .truncate()
            .sense(Sense::click()),
    );
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect.expand(2.0),
            2.0,
            Stroke::new(1.0_f32, Color32::from_gray(110)),
            egui::StrokeKind::Outside,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn copy_to_clipboard(ui: &egui::Ui, text: &str) {
    crate::clipboard::copy(ui.ctx(), text);
}

fn format_numeric(data: &InspectorData, kind: &str, endian: Endianness) -> String {
    let le = matches!(endian, Endianness::Little);
    match kind {
        "u8" => data.u8_val.map_or_else(|| "-".into(), |v| v.to_string()),
        "i8" => data.i8_val.map_or_else(|| "-".into(), |v| v.to_string()),
        "u16" => (if le { data.u16_le } else { data.u16_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "i16" => (if le { data.i16_le } else { data.i16_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "u32" => (if le { data.u32_le } else { data.u32_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "i32" => (if le { data.i32_le } else { data.i32_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "u64" => (if le { data.u64_le } else { data.u64_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "i64" => (if le { data.i64_le } else { data.i64_be })
            .map_or_else(|| "-".into(), |v| v.to_string()),
        "f32" => (if le { data.f32_le } else { data.f32_be })
            .map_or_else(|| "-".into(), |v| format!("{v:.6}")),
        "f64" => (if le { data.f64_le } else { data.f64_be })
            .map_or_else(|| "-".into(), |v| format!("{v:.6}")),
        _ => "-".into(),
    }
}

pub fn render_inspector_panel(
    ui: &mut egui::Ui,
    doc: &mut Document,
    state: &mut InspectorUiState,
    plugins: &[Plugin],
    plugin_results: &HashMap<String, String>,
    plugins_running: bool,
    trigger_plugins: &mut bool,
    reload_plugins: &mut bool,
) {
    let window = doc.inspector_window_bytes(64);
    let data = InspectorData::decode(&window, doc.selection);
    let rs = data.range_start;

    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.endianness, Endianness::Little, "Little Endian");
        ui.selectable_value(&mut state.endianness, Endianness::Big, "Big Endian");
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("inspector_scroll")
        .animated(false)
        .drag_to_scroll(false)
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            egui::Grid::new("inspector_general")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    let range = format!("0x{:X}..0x{:X}", data.range_start, data.range_end);
                    let uleb_str = data
                        .uleb128
                        .map_or_else(|| "-".into(), |(v, n)| format!("{v} ({n}B)"));
                    let sleb_str = data
                        .sleb128
                        .map_or_else(|| "-".into(), |(v, n)| format!("{v} ({n}B)"));
                    let ts_str = data
                        .unix_timestamp
                        .clone()
                        .unwrap_or_else(|| "-".into());
                    let rows: Vec<(&str, String, Option<&str>)> = vec![
                        ("Range", range, None),
                        ("Length", format!("{} bytes", data.selection_len), None),
                        ("Hex", data.hex_preview.clone(), Some("hex")),
                        ("Decimal", data.decimal_preview.clone(), None),
                        ("ASCII", data.ascii_preview.clone(), None),
                        ("Binary", data.binary_preview.clone(), None),
                        ("UTF-8", data.utf8_preview.clone(), Some("utf8")),
                        ("UTF-16LE", data.utf16le_preview.clone(), Some("utf16le")),
                        ("ULEB128", uleb_str, None),
                        ("SLEB128", sleb_str, None),
                        ("Unix Time", ts_str, None),
                    ];
                    for (label, value, kind) in rows {
                        value_row(ui, doc, state, rs, label, &value, kind);
                    }
                });

            ui.add_space(6.0);
            ui.strong("Numeric Values");
            egui::Grid::new("inspector_numeric")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for kind in [
                        "u8", "i8", "u16", "i16", "u32", "i32", "u64", "i64", "f32", "f64",
                    ] {
                        let value = format_numeric(&data, kind, state.endianness);
                        value_row(ui, doc, state, rs, kind, &value, Some(kind));
                    }
                });

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Inspector Plugins");
                if ui
                    .add_enabled(
                        !plugins.is_empty() && !plugins_running,
                        egui::Button::new("▶ Run (r)").small(),
                    )
                    .on_hover_text("Execute all inspector scripts against the current selection")
                    .clicked()
                {
                    *trigger_plugins = true;
                }
                if ui
                    .small_button("🔄")
                    .on_hover_text("Rescan inspector plugins directory")
                    .clicked()
                {
                    *reload_plugins = true;
                }
                if ui
                    .small_button("📂")
                    .on_hover_text("Open inspector plugins directory in file manager")
                    .clicked()
                {
                    let dir = rex_core::plugins::default_directory();
                    let _ = std::fs::create_dir_all(&dir);
                    #[cfg(windows)]
                    {
                        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    }
                    #[cfg(unix)]
                    {
                        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                    }
                    doc.status = format!("Plugins directory: {}", dir.display());
                }
                if plugins_running {
                    ui.spinner();
                }
            });
            if plugins.is_empty() {
                ui.weak("No scripts in inspectors folder (.py, .sh, .ps1, .bat, .exe). Click 📂 to add.");
            } else {
                egui::Grid::new("inspector_plugins")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for p in plugins {
                            ui.label(&p.name);
                            let res = plugin_results
                                .get(&p.name)
                                .map(String::as_str)
                                .unwrap_or(if plugins_running {
                                    PENDING_PLACEHOLDER
                                } else {
                                    NOT_RUN_PLACEHOLDER
                                });
                            ui.horizontal(|ui| {
                                if res == ERROR_PLACEHOLDER {
                                    ui.colored_label(Color32::LIGHT_RED, res);
                                } else if res == PENDING_PLACEHOLDER || res == NOT_RUN_PLACEHOLDER {
                                    ui.weak(res);
                                } else {
                                    let disp = truncate_display(res, 22);
                                    let lbl = copyable_value(ui, &disp);
                                    let lbl = if disp != res {
                                        lbl.on_hover_text(format!("{res}\n\nClick to copy"))
                                    } else {
                                        lbl.on_hover_text("Click to copy")
                                    };
                                    if lbl.clicked() {
                                        copy_to_clipboard(ui, res);
                                        doc.status = format!("Copied {}: {disp}", p.name);
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
            }
        });
}

/// Swatch picker for one optional highlight style; the first swatch means "no color".
fn color_selector(ui: &mut egui::Ui, id: &str, current: &mut Option<FieldColor>) {
    ui.horizontal_wrapped(|ui| {
        ui.push_id(id, |ui| {
            let choices = std::iter::once(None).chain(FieldColor::ALL.into_iter().map(Some));
            for choice in choices {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                let painter = ui.painter();
                match choice {
                    Some(c) => {
                        painter.rect_filled(rect, 3.0, field_color32(c));
                    }
                    None => {
                        painter.rect_stroke(
                            rect.shrink(1.0),
                            3.0,
                            Stroke::new(1.0_f32, Color32::from_gray(150)),
                            egui::StrokeKind::Inside,
                        );
                        painter.line_segment(
                            [rect.left_bottom() + Vec2::new(3.0, -3.0), rect.right_top() + Vec2::new(-3.0, 3.0)],
                            Stroke::new(1.5_f32, Color32::from_rgb(230, 90, 90)),
                        );
                    }
                }
                if *current == choice {
                    painter.rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(2.0_f32, Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
                let tip = choice.map_or("No color", FieldColor::name);
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(tip)
                    .clicked()
                {
                    *current = choice;
                }
            }
        });
    });
}

/// Small preview of a field's highlight styles (background, border and text color).
fn field_swatch(ui: &mut egui::Ui, f: &Field) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(18.0, 14.0), Sense::hover());
    let painter = ui.painter();
    if let Some(bg) = f.background {
        painter.rect_filled(rect, 2.0, field_color32(bg).gamma_multiply(0.6));
    }
    if let Some(border) = f.border {
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.5_f32, field_color32(border)),
            egui::StrokeKind::Inside,
        );
    }
    let text = f.text.map_or(Color32::from_gray(200), field_color32);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "A",
        egui::FontId::monospace(10.0),
        text,
    );
}

pub fn render_fields_panel(ui: &mut egui::Ui, doc: &mut Document, editor: &mut FieldEditorState) {
    ui.horizontal(|ui| {
        if ui.button("+ Add Field (a)").clicked() {
            editor.begin_add_for_selection(doc);
        }
        let has_sel = doc.selected_field < doc.fields.len();
        if ui.add_enabled(has_sel, egui::Button::new("Edit")).clicked() {
            let idx = doc.selected_field;
            let f = &doc.fields[idx];
            let filter = std::mem::take(&mut editor.filter);
            *editor = FieldEditorState {
                open: true,
                editing_index: Some(idx),
                name: f.name.clone(),
                description: f.description.clone(),
                start_str: format!("0x{:X}", f.start),
                end_str: format!("0x{:X}", f.end),
                border: f.border,
                text: f.text,
                background: f.background,
                visible: f.visible,
                filter,
            };
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Delete"))
            .clicked()
        {
            let idx = doc.selected_field;
            doc.delete_field(idx);
            editor.open = false;
        }
        if doc.fields.len() > 1 {
            if ui
                .small_button("◀")
                .on_hover_text("Previous field ([)")
                .clicked()
            {
                let prev = doc
                    .selected_field
                    .checked_sub(1)
                    .unwrap_or(doc.fields.len() - 1);
                doc.selected_field = prev;
                let (s, e) = (doc.fields[prev].start, doc.fields[prev].end);
                doc.select_offset(s, false);
                if let Some(sel) = &mut doc.selection {
                    sel.cursor = e;
                }
            }
            if ui
                .small_button("▶")
                .on_hover_text("Next field (])")
                .clicked()
            {
                let next = (doc.selected_field + 1) % doc.fields.len();
                doc.selected_field = next;
                let (s, e) = (doc.fields[next].start, doc.fields[next].end);
                doc.select_offset(s, false);
                if let Some(sel) = &mut doc.selection {
                    sel.cursor = e;
                }
            }
        }
    });
    if !doc.fields.is_empty() {
        ui.add(
            egui::TextEdit::singleline(&mut editor.filter)
                .hint_text("Filter fields by name or description...")
                .desired_width(ui.available_width()),
        );
    }
    ui.separator();

    let needle = editor.filter.trim().to_ascii_lowercase();
    let mut clicked: Option<usize> = None;
    let mut visibility_toggled: Option<(usize, bool)> = None;
    egui::ScrollArea::vertical()
        .max_height(if editor.open { 150.0 } else { 220.0 })
        .id_salt("fields_list")
        .animated(false)
        .drag_to_scroll(false)
        .show(ui, |ui| {
            for (idx, f) in doc.fields.iter().enumerate() {
                if !needle.is_empty()
                    && !f.name.to_ascii_lowercase().contains(&needle)
                    && !f.description.to_ascii_lowercase().contains(&needle)
                {
                    continue;
                }
                let selected = idx == doc.selected_field;
                let label = format!(
                    "{}  (0x{:X}..0x{:X}, {}B)",
                    f.name,
                    f.start,
                    f.end,
                    f.len()
                );
                let resp = ui
                    .horizontal(|ui| {
                        let mut visible = f.visible;
                        if ui
                            .checkbox(&mut visible, "")
                            .on_hover_text("Show or hide this field in the hex view")
                            .changed()
                        {
                            visibility_toggled = Some((idx, visible));
                        }
                        field_swatch(ui, f);
                        let text = if f.visible {
                            egui::RichText::new(label)
                        } else {
                            egui::RichText::new(label).weak()
                        };
                        ui.selectable_label(selected, text)
                    })
                    .inner;
                if !f.description.is_empty() {
                    resp.clone().on_hover_text(&f.description);
                }
                if resp.clicked() {
                    clicked = Some(idx);
                }
            }
            if doc.fields.is_empty() {
                ui.weak("No fields defined. Select bytes and click '+ Add Field' (or press 'a').");
            }
        });
    if let Some((idx, visible)) = visibility_toggled {
        doc.fields[idx].visible = visible;
        let _ = doc.persist_automatic_overlay();
    }
    if let Some(idx) = clicked {
        let (start, end) = (doc.fields[idx].start, doc.fields[idx].end);
        doc.selected_field = idx;
        doc.select_offset(start, false);
        if let Some(sel) = &mut doc.selection {
            sel.cursor = end;
        }
    }

    if !editor.open {
        return;
    }
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(if editor.editing_index.is_some() {
            "Edit Field"
        } else {
            "New Field"
        });
        egui::Grid::new("field_editor_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut editor.name);
                ui.end_row();
                ui.label("Description");
                ui.text_edit_singleline(&mut editor.description);
                ui.end_row();
                ui.label("Start");
                ui.text_edit_singleline(&mut editor.start_str);
                ui.end_row();
                ui.label("End");
                ui.text_edit_singleline(&mut editor.end_str);
                ui.end_row();
            });
        ui.label("Border Color");
        color_selector(ui, "border", &mut editor.border);
        ui.label("Text Color");
        color_selector(ui, "text", &mut editor.text);
        ui.label("Background Color");
        color_selector(ui, "background", &mut editor.background);
        let parsed = Document::parse_offset(&editor.start_str)
            .ok()
            .zip(Document::parse_offset(&editor.end_str).ok());
        ui.horizontal(|ui| {
            let ok = parsed.is_some() && !editor.name.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Save")).clicked()
                && let Some((start, end)) = parsed
            {
                let field = Field {
                    name: editor.name.trim().to_string(),
                    description: editor.description.clone(),
                    start: start.min(end),
                    end: start.max(end),
                    border: editor.border,
                    text: editor.text,
                    background: editor.background,
                    visible: editor.visible,
                };
                match editor.editing_index {
                    Some(i) => doc.update_field(i, field),
                    None => doc.add_field(field),
                }
                let _ = doc.persist_automatic_overlay();
                editor.open = false;
            }
            if ui.button("Cancel").clicked() {
                editor.open = false;
            }
        });
    });
}

pub fn render_entropy_panel(
    ui: &mut egui::Ui,
    doc: &mut Document,
    report: Option<&EntropyReport>,
    scanned: usize,
    total: usize,
) {
    let Some(rep) = report else {
        let frac = if total == 0 {
            0.0
        } else {
            scanned as f32 / total as f32
        };
        ui.label("Scanning entropy...");
        ui.add(egui::ProgressBar::new(frac).show_percentage());
        return;
    };

    ui.horizontal(|ui| {
        ui.strong(format!(
            "Avg: {:.3}  Min: {:.3}  Max: {:.3} bits/byte",
            rep.average, rep.min, rep.max
        ));
        if !doc.buffer.is_empty()
            && ui
                .small_button(format!("Jump to Peak (0x{:X})", rep.peak_offset))
                .on_hover_text("Jump to the bucket with the highest Shannon entropy")
                .clicked()
        {
            doc.select_offset(rep.peak_offset, false);
        }
        ui.weak("(Click or drag chart to scrub file offset)");
    });
    ui.add_space(4.0);

    let buckets = &rep.buckets;
    let n = buckets.len().max(1);
    let (resp, painter) = ui.allocate_painter(
        Vec2::new(ui.available_width(), 110.0),
        Sense::click_and_drag(),
    );
    let rect = resp.rect;
    painter.rect_filled(rect, 2.0, Color32::from_gray(20));
    let bar_w = rect.width() / n as f32;
    for (i, v) in buckets.iter().enumerate() {
        let h = (*v as f32 / 8.0).clamp(0.0, 1.0) * rect.height();
        let x = rect.left() + i as f32 * bar_w;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - h),
            egui::pos2(x + bar_w.max(1.0), rect.bottom()),
        );
        let t = (*v as f32 / 8.0).clamp(0.0, 1.0);
        let color = Color32::from_rgb((60.0 + 195.0 * t) as u8, (180.0 - 110.0 * t) as u8, 90);
        painter.rect_filled(bar, 0.0, color);
    }

    // Draw live cursor position marker on the entropy minimap
    if !doc.buffer.is_empty()
        && let Some(sel) = &doc.selection
    {
        let cursor_ratio =
            (sel.cursor as f64 / doc.buffer.len().max(1) as f64).clamp(0.0, 1.0) as f32;
        let cx = rect.left() + cursor_ratio * rect.width();
        painter.vline(
            cx,
            rect.y_range(),
            Stroke::new(1.5_f32, Color32::from_rgb(80, 220, 255)),
        );
    }

    if let Some(pos) = resp.hover_pos() {
        let ratio = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        painter.vline(pos.x, rect.y_range(), Stroke::new(1.0_f32, Color32::WHITE));
        let off = (ratio as f64 * doc.buffer.len() as f64) as usize;
        let bucket_idx = ((ratio * n as f32) as usize).min(n.saturating_sub(1));
        let ent = buckets.get(bucket_idx).copied().unwrap_or(0.0);
        resp.clone()
            .on_hover_text(format!("Offset 0x{off:X} ({ent:.2} bits/byte)"));
    }
    if (resp.clicked() || resp.dragged())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let ratio = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let off = ((ratio as f64 * doc.buffer.len() as f64) as usize)
            .min(doc.buffer.len().saturating_sub(1));
        doc.select_offset(off, false);
    }

    ui.add_space(6.0);
    ui.strong("Byte Class Distribution");
    let h = &rep.histogram;
    let sum = |r: std::ops::RangeInclusive<usize>| -> u64 { r.map(|i| h[i]).sum() };
    let null = h[0];
    let printable = sum(0x20..=0x7E);
    let control = sum(0x01..=0x1F) + h[0x7F];
    let high = sum(0x80..=0xFF);
    let total_b = (null + printable + control + high).max(1) as f32;
    ui.horizontal_wrapped(|ui| {
        for (label, count, color) in [
            ("Null (0x00)", null, Color32::from_gray(120)),
            ("Printable ASCII", printable, Color32::from_rgb(90, 180, 110)),
            ("Control", control, Color32::from_rgb(220, 170, 70)),
            ("High (0x80..0xFF)", high, Color32::from_rgb(200, 90, 90)),
        ] {
            ui.label(label);
            ui.add(
                egui::ProgressBar::new(count as f32 / total_b)
                    .fill(color)
                    .desired_width(95.0)
                    .text(format!("{:.1}%", 100.0 * count as f32 / total_b)),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Harness, copied_text, find_text};
    use egui::{FullOutput, Pos2};

    fn inspector_frame(
        h: &mut Harness,
        doc: &mut Document,
        state: &mut InspectorUiState,
        value: &str,
        kind: Option<&str>,
    ) -> (FullOutput, egui::Rect) {
        let mut rect = egui::Rect::NOTHING;
        let out = h.frame(|ui| {
            egui::Grid::new("g").show(ui, |ui| {
                rect = value_row(ui, doc, state, 0, "u8", value, kind).rect;
            });
        });
        (out, rect)
    }

    fn click(h: &mut Harness, doc: &mut Document, state: &mut InspectorUiState, at: Pos2, value: &str, kind: Option<&str>) -> Option<String> {
        h.press(at);
        let (a, _) = inspector_frame(h, doc, state, value, kind);
        h.release(at);
        let (b, _) = inspector_frame(h, doc, state, value, kind);
        copied_text(&a).or_else(|| copied_text(&b))
    }

    fn setup() -> (Harness, Document, InspectorUiState) {
        let mut doc = Document::from_bytes("t.bin".into(), vec![68, 0, 0, 0]);
        doc.select_offset(0, false);
        (Harness::new(500.0, 200.0), doc, InspectorUiState::default())
    }

    #[test]
    fn single_click_copies_the_value() {
        let (mut h, mut doc, mut state) = setup();
        let (_, rect) = inspector_frame(&mut h, &mut doc, &mut state, "68", Some("u8"));
        let copied = click(&mut h, &mut doc, &mut state, rect.center(), "68", Some("u8"));
        assert_eq!(copied.as_deref(), Some("68"));
        assert!(doc.status.starts_with("Copied u8"), "{}", doc.status);
        assert!(state.editing_kind.is_none(), "a single click does not start editing");
    }

    #[test]
    fn placeholder_values_are_not_copied() {
        let (mut h, mut doc, mut state) = setup();
        let (_, rect) = inspector_frame(&mut h, &mut doc, &mut state, "-", None);
        assert_eq!(click(&mut h, &mut doc, &mut state, rect.center(), "-", None), None);
    }

    fn start_edit(h: &mut Harness, doc: &mut Document, state: &mut InspectorUiState) {
        let (_, rect) = inspector_frame(h, doc, state, "68", Some("u8"));
        click(h, doc, state, rect.center(), "68", Some("u8"));
        click(h, doc, state, rect.center(), "68", Some("u8"));
        assert_eq!(state.editing_kind.as_deref(), Some("u8"), "double-click starts editing");
        assert_eq!(state.edit_buffer, "68");
        // Next frame shows the focused text box.
        inspector_frame(h, doc, state, "68", Some("u8"));
    }

    #[test]
    fn double_click_edits_and_enter_writes() {
        let (mut h, mut doc, mut state) = setup();
        start_edit(&mut h, &mut doc, &mut state);
        state.edit_buffer = "255".into();
        h.key(egui::Key::Enter);
        inspector_frame(&mut h, &mut doc, &mut state, "68", Some("u8"));
        assert_eq!(doc.buffer.get(0), Some(255));
        assert!(state.editing_kind.is_none());
    }

    #[test]
    fn escape_cancels_an_edit() {
        let (mut h, mut doc, mut state) = setup();
        start_edit(&mut h, &mut doc, &mut state);
        state.edit_buffer = "1".into();
        h.key(egui::Key::Escape);
        inspector_frame(&mut h, &mut doc, &mut state, "68", Some("u8"));
        assert_eq!(doc.buffer.get(0), Some(68));
        assert!(state.editing_kind.is_none());
    }

    #[test]
    fn read_only_values_ignore_double_click() {
        let (mut h, mut doc, mut state) = setup();
        let (_, rect) = inspector_frame(&mut h, &mut doc, &mut state, "68", None);
        click(&mut h, &mut doc, &mut state, rect.center(), "68", None);
        click(&mut h, &mut doc, &mut state, rect.center(), "68", None);
        assert!(state.editing_kind.is_none());
    }

    #[test]
    fn new_fields_default_to_a_rotating_border_only() {
        let mut doc = Document::from_bytes("t.bin".into(), vec![0; 16]);
        doc.select_offset(2, false);
        let mut editor = FieldEditorState::default();
        editor.begin_add_for_selection(&doc);
        assert_eq!(editor.border, Some(FieldColor::ALL[0]));
        assert_eq!((editor.text, editor.background), (None, None));
        assert!(editor.visible);

        doc.add_field(Field::default());
        editor.begin_add_for_selection(&doc);
        assert_eq!(editor.border, Some(FieldColor::ALL[1]));
    }

    fn fields_frame(h: &mut Harness, doc: &mut Document, editor: &mut FieldEditorState) -> FullOutput {
        h.frame(|ui| render_fields_panel(ui, doc, editor))
    }

    fn click_at(h: &mut Harness, doc: &mut Document, editor: &mut FieldEditorState, at: Pos2) -> FullOutput {
        h.press(at);
        fields_frame(h, doc, editor);
        h.release(at);
        fields_frame(h, doc, editor)
    }

    #[test]
    fn editor_can_clear_border_and_pick_text_and_background() {
        let mut doc = Document::from_bytes("t.bin".into(), vec![0; 16]);
        doc.select_offset(2, false);
        let mut editor = FieldEditorState::default();
        editor.begin_add_for_selection(&doc);
        let mut h = Harness::new(420.0, 700.0);
        fields_frame(&mut h, &mut doc, &mut editor);
        let out = fields_frame(&mut h, &mut doc, &mut editor);

        // Each picker row starts with the "no color" swatch, followed by the colors.
        let swatch = |out: &FullOutput, label: &str, index: usize| {
            let r = find_text(out, label).unwrap_or_else(|| panic!("{label} not drawn"));
            Pos2::new(r.left() + 8.0 + index as f32 * 20.0, r.bottom() + 12.0)
        };
        let out = click_at(&mut h, &mut doc, &mut editor, swatch(&out, "Border Color", 0));
        assert_eq!(editor.border, None);
        let out = click_at(&mut h, &mut doc, &mut editor, swatch(&out, "Text Color", 2));
        assert_eq!(editor.text, Some(FieldColor::ALL[1]));
        let out = click_at(&mut h, &mut doc, &mut editor, swatch(&out, "Background Color", 3));
        assert_eq!(editor.background, Some(FieldColor::ALL[2]));

        let save = find_text(&out, "Save").expect("Save button").center();
        click_at(&mut h, &mut doc, &mut editor, save);
        assert_eq!(doc.fields.len(), 1);
        let f = &doc.fields[0];
        assert_eq!((f.border, f.text, f.background), (None, Some(FieldColor::ALL[1]), Some(FieldColor::ALL[2])));
        assert!(f.visible);
    }

    #[test]
    fn field_list_checkbox_toggles_visibility() {
        let mut doc = Document::from_bytes("t.bin".into(), vec![0; 16]);
        doc.add_field(Field {
            name: "magic".into(),
            start: 0,
            end: 3,
            ..Default::default()
        });
        let mut editor = FieldEditorState::default();
        let mut h = Harness::new(420.0, 400.0);
        fields_frame(&mut h, &mut doc, &mut editor);
        let out = fields_frame(&mut h, &mut doc, &mut editor);
        let label = find_text(&out, "magic").expect("field row");
        // The checkbox is the first widget on the row, at the panel's left edge.
        let checkbox = Pos2::new(16.0, label.center().y);

        click_at(&mut h, &mut doc, &mut editor, checkbox);
        assert!(!doc.fields[0].visible);
        click_at(&mut h, &mut doc, &mut editor, checkbox);
        assert!(doc.fields[0].visible);
    }
}
