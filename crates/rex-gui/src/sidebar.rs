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
}

#[derive(Default)]
pub struct FieldEditorState {
    pub open: bool,
    pub editing_index: Option<usize>,
    pub name: String,
    pub description: String,
    pub start_str: String,
    pub end_str: String,
    pub color: FieldColor,
    pub background: Option<FieldColor>,
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

/// Renders one inspector row. `kind` (if `Some`) enables inline editing.
fn value_row(
    ui: &mut egui::Ui,
    doc: &mut Document,
    state: &mut InspectorUiState,
    range_start: usize,
    label: &str,
    value: &str,
    kind: Option<&str>,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        let editing = kind.is_some() && state.editing_kind.as_deref() == kind;
        if editing {
            let resp =
                ui.add(egui::TextEdit::singleline(&mut state.edit_buffer).desired_width(110.0));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let apply = ui.small_button("Apply").clicked() || enter;
            if ui.small_button("X").clicked() {
                state.editing_kind = None;
            } else if apply {
                if let Some(k) = kind {
                    match InspectorValue::parse_as(k, &state.edit_buffer, state.endianness) {
                        Ok(bytes) => doc.overwrite_bytes(range_start, &bytes),
                        Err(err) => doc.status = err,
                    }
                }
                state.editing_kind = None;
            }
        } else {
            let display = truncate_display(value, 22);
            let lbl = ui.add(
                egui::Label::new(egui::RichText::new(&display).monospace()).truncate(),
            );
            if display != value {
                lbl.on_hover_text(value);
            }
            if value != "-"
                && !value.is_empty()
                && ui
                    .small_button("📋")
                    .on_hover_text("Copy value to clipboard")
                    .clicked()
                && let Ok(mut cb) = arboard::Clipboard::new()
            {
                let _ = cb.set_text(value.to_string());
                doc.status = format!("Copied {label}: {display}");
            }
            if let Some(k) = kind
                && ui.small_button("Edit").clicked()
            {
                state.editing_kind = Some(k.to_string());
                state.edit_buffer = value.to_string();
            }
        }
    });
    ui.end_row();
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
                                    let lbl = ui.add(
                                        egui::Label::new(egui::RichText::new(&disp).monospace())
                                            .truncate(),
                                    );
                                    if disp != res {
                                        lbl.on_hover_text(res);
                                    }
                                    if ui
                                        .small_button("📋")
                                        .on_hover_text("Copy plugin output")
                                        .clicked()
                                        && let Ok(mut cb) = arboard::Clipboard::new()
                                    {
                                        let _ = cb.set_text(res.to_string());
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

fn color_selector(ui: &mut egui::Ui, id: &str, current: &mut FieldColor) {
    ui.horizontal_wrapped(|ui| {
        ui.push_id(id, |ui| {
            for c in FieldColor::ALL {
                let selected = *current == c;
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                ui.painter().rect_filled(rect, 3.0, field_color32(c));
                if selected {
                    ui.painter().rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(2.0_f32, Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
                if resp.on_hover_text(c.name()).clicked() {
                    *current = c;
                }
            }
        });
    });
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
                color: f.color,
                background: f.background,
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
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, field_color32(f.color));
                        ui.selectable_label(selected, label)
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
        ui.label("Foreground Color");
        color_selector(ui, "fg", &mut editor.color);
        ui.horizontal(|ui| {
            let mut has_bg = editor.background.is_some();
            if ui.checkbox(&mut has_bg, "Background Shading").changed() {
                editor.background = if has_bg { Some(editor.color) } else { None };
            }
        });
        if let Some(bg) = &mut editor.background {
            color_selector(ui, "bg", bg);
        }
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
                    color: editor.color,
                    background: editor.background,
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
