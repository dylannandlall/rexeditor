//! Drawing for the terminal UI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use rex_core::{
    DisplayRow, Document, EditKind, Endianness, Field, FieldColor, InspectorData, NamedColor,
    SearchMatch,
};

use super::{App, Column, Focus, HexGeometry, Tab};

const PANEL_MIN_W: u16 = 36;
const PANEL_MAX_W: u16 = 48;
const AUTO_BPR: [usize; 5] = [32, 24, 16, 8, 4];

pub struct FrameLayout {
    pub tabs: Rect,
    pub hex_block: Rect,
    pub panel: Option<Rect>,
    pub status: Rect,
    pub hint: Rect,
    pub geometry: HexGeometry,
}

fn offset_width(doc: Option<&Document>) -> u16 {
    let max = doc.map_or(0, |d| d.buffer.len().saturating_sub(1));
    let digits = if max == 0 {
        1
    } else {
        (usize::BITS - max.leading_zeros()).div_ceil(4)
    };
    digits.max(8) as u16
}

/// Splits the screen and chooses bytes-per-row so the hex view (and, when there is
/// room, the side panel) fit the terminal.
pub fn compute_layout(app: &App, area: Rect) -> FrameLayout {
    let [tabs, body, status, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let off_w = offset_width(app.tab().map(|t| &t.doc));
    let block_w = |bpr: usize| HexGeometry::width(off_w, bpr) + 2;
    let fits = |bpr: usize, w: u16| block_w(bpr) <= w;
    let w = body.width;
    let with_panel = w.saturating_sub(PANEL_MIN_W);

    let (bpr, panel) = match app.manual_bpr {
        Some(m) => (m, app.show_panel && fits(m, with_panel)),
        None => {
            let wide = if app.show_panel {
                AUTO_BPR[..3].iter().copied().find(|&b| fits(b, with_panel))
            } else {
                None
            };
            match wide {
                Some(b) => (b, true),
                None => {
                    let b = AUTO_BPR.iter().copied().find(|&b| fits(b, w)).unwrap_or(4);
                    // Keep the panel next to a narrow (8-byte) view only if the
                    // full-width view would not be any wider.
                    let keep_panel = app.show_panel && b <= 8 && fits(b, with_panel);
                    (b, keep_panel)
                }
            }
        }
    };

    let hex_w = block_w(bpr).min(w);
    let (hex_block, panel) = if panel {
        let panel_w = (w - hex_w)
            .min(PANEL_MAX_W.max(w.saturating_sub(hex_w) / 2))
            .max(PANEL_MIN_W);
        let panel_w = panel_w.min(w - hex_w);
        (
            Rect::new(body.x, body.y, w - panel_w, body.height),
            Some(Rect::new(
                body.x + w - panel_w,
                body.y,
                panel_w,
                body.height,
            )),
        )
    } else {
        (body, None)
    };

    // Inside the border, the first line holds column headers.
    let inner = Block::bordered().inner(hex_block);
    let rows_area = Rect::new(
        inner.x,
        inner.y + 1.min(inner.height),
        inner.width,
        inner.height.saturating_sub(1),
    );
    FrameLayout {
        tabs,
        hex_block,
        panel,
        status,
        hint,
        geometry: HexGeometry {
            area: rows_area,
            off_w,
            bpr,
        },
    }
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let layout = compute_layout(app, frame.area());
    render_tabs(frame, app, layout.tabs);
    match app.tabs.get(app.active) {
        Some(tab) => render_hex(frame, app, tab, &layout),
        None => render_welcome(frame, layout.hex_block),
    }
    app.fields_geo = Default::default();
    if let Some(panel) = layout.panel
        && let Some(tab) = app.tabs.get(app.active)
    {
        let geo = render_panel(frame, app, tab, panel);
        app.fields_geo = geo;
    }
    render_status(frame, app, layout.status);
    render_hint(frame, app, layout.hint);
    if app.show_help {
        render_help(frame, app);
    }
}

// ---------------------------------------------------------------------------
// Colors

fn named(c: NamedColor, truecolor: bool) -> Color {
    if truecolor {
        let rgba = c.rgba();
        return Color::Rgb(rgba.0, rgba.1, rgba.2);
    }
    match c {
        NamedColor::Black => Color::Black,
        NamedColor::DarkGray => Color::DarkGray,
        NamedColor::Gray => Color::Gray,
        NamedColor::White => Color::White,
        NamedColor::Red => Color::Red,
        NamedColor::LightRed => Color::LightRed,
        NamedColor::Green => Color::Green,
        NamedColor::LightGreen => Color::LightGreen,
        NamedColor::Yellow => Color::Yellow,
        NamedColor::LightYellow => Color::LightYellow,
        NamedColor::Blue => Color::Blue,
        NamedColor::LightBlue => Color::LightBlue,
        NamedColor::Magenta => Color::Magenta,
        NamedColor::LightMagenta => Color::LightMagenta,
        NamedColor::Cyan => Color::Cyan,
        NamedColor::LightCyan => Color::LightCyan,
    }
}

fn field_color(c: FieldColor, truecolor: bool) -> Color {
    if truecolor {
        let rgba = c.rgba();
        return Color::Rgb(rgba.0, rgba.1, rgba.2);
    }
    match c {
        FieldColor::Cyan => Color::Cyan,
        FieldColor::Green => Color::Green,
        FieldColor::Yellow => Color::Yellow,
        FieldColor::Magenta => Color::Magenta,
        FieldColor::Blue => Color::Blue,
        FieldColor::Red => Color::Red,
        FieldColor::LightCyan => Color::LightCyan,
        FieldColor::LightGreen => Color::LightGreen,
        FieldColor::LightYellow => Color::LightYellow,
        FieldColor::LightMagenta => Color::LightMagenta,
        FieldColor::LightBlue => Color::LightBlue,
        FieldColor::LightRed => Color::LightRed,
        FieldColor::White => Color::White,
        FieldColor::Gray => Color::Gray,
    }
}

/// Background fill for a field: dimmed in truecolor so the text stays readable.
fn field_background(c: FieldColor, truecolor: bool) -> Color {
    if truecolor {
        let rgba = c.rgba();
        let dim = |v: u8| (v as f32 * 0.45) as u8;
        Color::Rgb(dim(rgba.0), dim(rgba.1), dim(rgba.2))
    } else {
        field_color(c, false)
    }
}

/// Applies a field's highlight styles. A border becomes a colored underline, the
/// closest a character cell can get to an outline.
fn apply_field_style(mut style: Style, f: &Field, truecolor: bool) -> Style {
    if let Some(bg) = f.background {
        style = style.bg(field_background(bg, truecolor));
        if !truecolor && f.text.is_none() {
            style = style.fg(Color::Black);
        }
    }
    if let Some(text) = f.text {
        style = style.fg(field_color(text, truecolor));
    }
    if let Some(border) = f.border {
        style = style
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(field_color(border, truecolor));
    }
    style
}

fn selection_style(truecolor: bool) -> Style {
    let bg = if truecolor {
        Color::Rgb(38, 79, 120)
    } else {
        Color::Blue
    };
    Style::new().bg(bg).fg(Color::White)
}

fn search_style(truecolor: bool) -> Style {
    if truecolor {
        Style::new().bg(Color::Rgb(110, 90, 20)).fg(Color::White)
    } else {
        Style::new().bg(Color::Yellow).fg(Color::Black)
    }
}

// ---------------------------------------------------------------------------
// Hex view

fn in_search_match(matches: &[SearchMatch], abs: usize) -> Option<usize> {
    let idx = matches.partition_point(|m| m.start <= abs);
    (idx > 0 && matches[idx - 1].contains(abs)).then(|| idx - 1)
}

fn visible_field(doc: &Document, abs: usize) -> Option<(usize, &Field)> {
    if !doc.settings.show_overlays {
        return None;
    }
    doc.fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.visible && f.contains(abs))
}

/// Style shared by a byte and, when its neighbour matches, the gap after it.
#[derive(Clone, Copy, PartialEq)]
enum Highlight {
    None,
    Selected,
    Search(usize),
    Field(usize),
}

fn highlight_at(doc: &Document, matches: &[SearchMatch], abs: usize) -> Highlight {
    if doc.is_selected(abs) {
        Highlight::Selected
    } else if let Some(i) = in_search_match(matches, abs) {
        Highlight::Search(i)
    } else if let Some((i, _)) = visible_field(doc, abs) {
        Highlight::Field(i)
    } else {
        Highlight::None
    }
}

fn render_welcome(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  RexEditor",
            Style::new().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  No file is open."),
        Line::from(""),
        Line::from("  o / Ctrl+O   open a file"),
        Line::from("  ?            keybindings"),
        Line::from("  q            quit"),
    ];
    frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
}

fn render_hex(frame: &mut Frame, app: &App, tab: &Tab, layout: &FrameLayout) {
    let doc = &tab.doc;
    let tc = app.truecolor;
    let geo = layout.geometry;
    let bpr = geo.bpr;
    let upper = doc.settings.uppercase_hex;
    let hex = |b: u8| {
        if upper {
            format!("{b:02X}")
        } else {
            format!("{b:02x}")
        }
    };

    let mode = if !doc.edit_mode {
        "VIEW"
    } else if doc.edit_kind == EditKind::Insert {
        "INSERT"
    } else {
        "OVERWRITE"
    };
    let dirty = if doc.is_dirty() { " [+]" } else { "" };
    let border_style = if app.focus == Focus::Hex {
        Style::new().fg(Color::Gray)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_style(border_style)
        .title(format!(" {}{dirty} ", doc.file_name()))
        .title_bottom(Line::from(format!(" {mode} ")).right_aligned());
    let inner = block.inner(layout.hex_block);
    frame.render_widget(block, layout.hex_block);
    if inner.height == 0 {
        return;
    }

    // Column header.
    let header_style = Style::new().fg(Color::DarkGray);
    let mut header = vec![Span::styled(
        format!("{:<w$}", "Offset", w = geo.off_w as usize + 2),
        header_style,
    )];
    let mut x = geo.off_w + 2;
    for i in 0..bpr {
        let hx = HexGeometry::hex_x(geo.off_w, i);
        header.push(Span::raw(" ".repeat((hx - x) as usize)));
        header.push(Span::styled(hex(i as u8), header_style));
        x = hx + 2;
    }
    let ax = HexGeometry::ascii_x(geo.off_w, bpr, 0);
    header.push(Span::raw(" ".repeat(ax.saturating_sub(x) as usize)));
    header.push(Span::styled("ASCII", header_style));
    frame.render_widget(
        Paragraph::new(Line::from(header)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let matches = &tab.search.matches;
    let cursor = doc.selection.map(|s| s.cursor);
    let pending = doc.pending_insert_nibble();
    let offset_style = Style::new().fg(named(doc.theme.offset, tc));
    let mut lines = Vec::with_capacity(geo.area.height as usize);
    let total_rows = doc.row_count();
    let mut row_buf = vec![0u8; bpr];

    for r in 0..geo.area.height as usize {
        let row = doc.scroll + r;
        if row >= total_rows {
            break;
        }
        match doc.display_row(row) {
            DisplayRow::Repeated {
                start,
                end,
                byte,
                physical_rows,
            } => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{start:0w$X}  ... {physical_rows} identical rows of 0x{byte:02X} (through {end:0w$X}) ...",
                        w = geo.off_w as usize
                    ),
                    offset_style.add_modifier(Modifier::ITALIC),
                )));
            }
            DisplayRow::Bytes { offset } => {
                let count = doc.buffer.copy_range(offset, &mut row_buf);
                let cursor_row = cursor.is_some_and(|c| (offset..offset + bpr).contains(&c));
                let off_text = if upper {
                    format!("{offset:0w$X}", w = geo.off_w as usize)
                } else {
                    format!("{offset:0w$x}", w = geo.off_w as usize)
                };
                let mut spans = vec![
                    Span::styled(
                        off_text,
                        if cursor_row {
                            offset_style.add_modifier(Modifier::BOLD).fg(Color::Gray)
                        } else {
                            offset_style
                        },
                    ),
                    Span::raw("  "),
                ];
                let mut ascii = Vec::with_capacity(bpr);
                let highlights: Vec<Highlight> = (0..count)
                    .map(|i| highlight_at(doc, matches, offset + i))
                    .collect();

                for i in 0..count {
                    let abs = offset + i;
                    let byte = row_buf[i];
                    let base_fg = if doc.buffer.is_modified(abs) {
                        named(doc.theme.modified, tc)
                    } else {
                        named(doc.theme.color_for_byte(abs, byte, false), tc)
                    };
                    let mut style = Style::new().fg(base_fg);
                    if let Some((_, f)) = visible_field(doc, abs) {
                        style = apply_field_style(style, f, tc);
                    }
                    match highlights[i] {
                        Highlight::Selected => style = style.patch(selection_style(tc)),
                        Highlight::Search(_) => style = style.patch(search_style(tc)),
                        _ => {}
                    }
                    let mut hex_style = style;
                    let mut ascii_style =
                        if doc.buffer.is_modified(abs) || highlights[i] != Highlight::None {
                            style
                        } else {
                            Style::new().fg(named(doc.theme.color_for_byte(abs, byte, true), tc))
                        };
                    if cursor == Some(abs) {
                        let (active, other) = match tab.column {
                            Column::Hex => (&mut hex_style, &mut ascii_style),
                            Column::Ascii => (&mut ascii_style, &mut hex_style),
                        };
                        *active = active.add_modifier(Modifier::REVERSED | Modifier::BOLD);
                        *other = other.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    }
                    let text = match pending {
                        Some((p, hi)) if p == abs => {
                            hex_style = hex_style.fg(Color::Yellow);
                            if upper {
                                format!("{hi:X}_")
                            } else {
                                format!("{hi:x}_")
                            }
                        }
                        _ => hex(byte),
                    };
                    spans.push(Span::styled(text, hex_style));

                    // Gap after the byte: continue the highlight when the next byte
                    // shares it, so ranges read as one block.
                    if i + 1 < bpr {
                        let gap = if (i + 1) % 8 == 0 { "  " } else { " " };
                        let gap_style = if i + 1 < count && highlights[i] == highlights[i + 1] {
                            match highlights[i] {
                                Highlight::Selected => selection_style(tc),
                                Highlight::Search(_) => search_style(tc),
                                Highlight::Field(fi) => {
                                    apply_field_style(Style::new(), &doc.fields[fi], tc)
                                }
                                Highlight::None => Style::new(),
                            }
                        } else {
                            Style::new()
                        };
                        spans.push(Span::styled(gap, gap_style));
                    }
                    let ch = if (0x20..0x7f).contains(&byte) {
                        byte as char
                    } else {
                        '.'
                    };
                    ascii.push(Span::styled(ch.to_string(), ascii_style));
                }
                // Pad short final rows so the ASCII column stays aligned.
                let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
                let ax = HexGeometry::ascii_x(geo.off_w, bpr, 0);
                spans.push(Span::raw(" ".repeat(ax.saturating_sub(used) as usize)));
                spans.extend(ascii);
                lines.push(Line::from(spans));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), geo.area);
}

// ---------------------------------------------------------------------------
// Side panel: inspector + overlay fields

fn numeric(data: &InspectorData, kind: &str, endian: Endianness) -> String {
    let le = endian == Endianness::Little;
    let pick = |l: Option<String>, b: Option<String>| if le { l } else { b };
    let v: Option<String> = match kind {
        "u8" => data.u8_val.map(|v| v.to_string()),
        "i8" => data.i8_val.map(|v| v.to_string()),
        "u16" => pick(
            data.u16_le.map(|v| v.to_string()),
            data.u16_be.map(|v| v.to_string()),
        ),
        "i16" => pick(
            data.i16_le.map(|v| v.to_string()),
            data.i16_be.map(|v| v.to_string()),
        ),
        "u32" => pick(
            data.u32_le.map(|v| v.to_string()),
            data.u32_be.map(|v| v.to_string()),
        ),
        "i32" => pick(
            data.i32_le.map(|v| v.to_string()),
            data.i32_be.map(|v| v.to_string()),
        ),
        "u64" => pick(
            data.u64_le.map(|v| v.to_string()),
            data.u64_be.map(|v| v.to_string()),
        ),
        "i64" => pick(
            data.i64_le.map(|v| v.to_string()),
            data.i64_be.map(|v| v.to_string()),
        ),
        "f32" => pick(
            data.f32_le.map(|v| format!("{v:.6}")),
            data.f32_be.map(|v| format!("{v:.6}")),
        ),
        "f64" => pick(
            data.f64_le.map(|v| format!("{v:.6}")),
            data.f64_be.map(|v| format!("{v:.6}")),
        ),
        _ => None,
    };
    v.unwrap_or_else(|| "-".into())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

fn render_panel(frame: &mut Frame, app: &App, tab: &Tab, area: Rect) -> super::FieldListGeometry {
    let doc = &tab.doc;
    let window = doc.inspector_window_bytes(64);
    let data = InspectorData::decode(&window, doc.selection);
    let endian = match app.endian {
        Endianness::Little => "LE",
        Endianness::Big => "BE",
    };

    let mut rows: Vec<(&str, String)> = vec![
        (
            "Range",
            format!("0x{:X}..0x{:X}", data.range_start, data.range_end),
        ),
        ("Length", format!("{} bytes", data.selection_len)),
        ("Hex", data.hex_preview.clone()),
        ("ASCII", data.ascii_preview.clone()),
        ("UTF-8", data.utf8_preview.clone()),
    ];
    for kind in [
        "u8", "i8", "u16", "i16", "u32", "i32", "u64", "i64", "f32", "f64",
    ] {
        rows.push((kind, numeric(&data, kind, app.endian)));
    }
    rows.push((
        "ULEB128",
        data.uleb128
            .map_or_else(|| "-".into(), |(v, n)| format!("{v} ({n}B)")),
    ));
    rows.push((
        "Unix time",
        data.unix_timestamp.clone().unwrap_or_else(|| "-".into()),
    ));

    let n_fields = doc.fields.len();
    let fields_wanted = (n_fields.max(1) + 2) as u16;
    let inspector_wanted = rows.len() as u16 + 2;
    let (insp_h, fields_h) = if area.height >= inspector_wanted + fields_wanted.min(6) {
        let f = (area.height - inspector_wanted).max(fields_wanted.min(6));
        (area.height - f, f)
    } else if app.focus == Focus::Fields {
        let f = (area.height / 2).max(3).min(area.height);
        (area.height - f, f)
    } else {
        let f = 3.min(area.height);
        (area.height - f, f)
    };
    let insp_area = Rect::new(area.x, area.y, area.width, insp_h);
    let fields_area = Rect::new(area.x, area.y + insp_h, area.width, fields_h);

    let label_w = 10usize;
    let value_w = (insp_area.width as usize).saturating_sub(label_w + 3);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(
                    format!("{label:<label_w$}"),
                    Style::new().fg(Color::DarkGray),
                ),
                Span::raw(truncate(value, value_w)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(Color::DarkGray))
                .title(format!(" Inspector ({endian}, e) ")),
        ),
        insp_area,
    );

    // Overlay fields list.
    let focused = app.focus == Focus::Fields;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new().fg(Color::DarkGray)
        })
        .title(format!(" Fields ({n_fields}) · f "));
    let inner = block.inner(fields_area);
    frame.render_widget(block, fields_area);
    if n_fields == 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Select bytes, press a",
                Style::new().fg(Color::DarkGray),
            ))),
            inner,
        );
        return super::FieldListGeometry::default();
    }
    let h = inner.height.max(1) as usize;
    let cursor = app.field_cursor.min(n_fields - 1);
    let first = if cursor >= h { cursor + 1 - h } else { 0 };
    let mut lines = Vec::new();
    for (i, f) in doc.fields.iter().enumerate().skip(first).take(h) {
        let check = if f.visible { "[x] " } else { "[ ] " };
        let swatch = apply_field_style(Style::new().fg(Color::Gray), f, app.truecolor);
        let mut name_style = if f.visible {
            Style::new()
        } else {
            Style::new().fg(Color::DarkGray)
        };
        if focused && i == cursor {
            name_style = name_style.add_modifier(Modifier::REVERSED);
        } else if i == doc.selected_field {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let range = format!(" {:X}..{:X}", f.start, f.end);
        let name_w = (inner.width as usize).saturating_sub(4 + 3 + range.len());
        lines.push(Line::from(vec![
            Span::raw(check),
            Span::styled("Ab", swatch),
            Span::raw(" "),
            Span::styled(truncate(&f.name, name_w), name_style),
            Span::styled(range, Style::new().fg(Color::DarkGray)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    super::FieldListGeometry {
        area: inner,
        first_index: first,
    }
}

// ---------------------------------------------------------------------------
// Tabs, status, hints, help

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " RexEditor ",
        Style::new()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    for (i, tab) in app.tabs.iter().enumerate() {
        let dirty = if tab.doc.is_dirty() { "*" } else { "" };
        let label = format!(" {}{dirty} ", tab.doc.file_name());
        let style = if i == app.active {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new().fg(Color::Gray)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, style));
    }
    if app.tabs.len() > 1 {
        spans.push(Span::styled(
            "  Alt+←/→ switch",
            Style::new().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    let help = " ? help ";
    if area.width as usize > help.len() {
        frame.render_widget(
            Paragraph::new(Span::styled(help, Style::new().fg(Color::DarkGray))),
            Rect::new(
                area.right() - help.len() as u16,
                area.y,
                help.len() as u16,
                1,
            ),
        );
    }
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let Some(tab) = app.tab() else {
        frame.render_widget(Paragraph::new(app.message.as_str()), area);
        return;
    };
    let doc = &tab.doc;
    let (badge, bg) = if !doc.edit_mode {
        ("VIEW", Color::Blue)
    } else if doc.edit_kind == EditKind::Insert {
        ("INSERT", Color::Yellow)
    } else {
        ("OVERWRITE", Color::Red)
    };
    let mut spans = vec![
        Span::styled(
            format!(" {badge} "),
            Style::new()
                .bg(bg)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            match tab.column {
                Column::Hex => " HEX ",
                Column::Ascii => " ASCII ",
            },
            Style::new().fg(Color::Black).bg(Color::Gray),
        ),
    ];
    if app.visual {
        spans.push(Span::styled(
            " VISUAL ",
            Style::new().fg(Color::Black).bg(Color::Magenta),
        ));
    }
    let pos = match doc.selection {
        Some(sel) if sel.anchor != sel.cursor => format!(
            " sel 0x{:X}..0x{:X} ({} bytes)",
            sel.start(),
            sel.end(),
            sel.len()
        ),
        Some(sel) => format!(" 0x{:X} ({})", sel.cursor, sel.cursor),
        None => " empty".into(),
    };
    spans.push(Span::raw(pos));
    spans.push(Span::styled(
        format!(" / 0x{:X}", doc.buffer.len()),
        Style::new().fg(Color::DarkGray),
    ));
    if tab.search.running {
        let pct = (tab.search.scanned * 100)
            .checked_div(tab.search.total)
            .unwrap_or(100);
        spans.push(Span::styled(
            format!("  searching {pct}%"),
            Style::new().fg(Color::Yellow),
        ));
    }
    let msg = if doc.status.is_empty() {
        app.message.as_str()
    } else {
        doc.status.as_str()
    };
    spans.push(Span::styled(
        format!("  {msg}"),
        Style::new().fg(Color::Gray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_hint(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(prompt) = &app.prompt {
        let label = format!("{}: ", prompt.kind.label());
        let hint = match prompt.kind {
            super::PromptKind::Search => {
                "   (hex DE AD ?? · text:abc · itext: · utf16le: · u32le:123 · re:[A-Z]+ · empty clears)"
            }
            super::PromptKind::Jump => "   (decimal or 0x hex)",
            _ => "",
        };
        let line = Line::from(vec![
            Span::styled(
                label.clone(),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(prompt.input.text.clone()),
            Span::styled(hint, Style::new().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        let before: usize = prompt
            .input
            .text
            .chars()
            .take(prompt.input.cursor)
            .map(unicode_width)
            .sum();
        let x = area.x + (label.len() + before) as u16;
        frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), area.y));
        return;
    }
    let hint = if app.tabs.is_empty() {
        "o open  ? help  q quit"
    } else if app.focus == Focus::Fields {
        "↑↓ choose  Space show/hide  b/t/c border/text/bg color (B/T/C back)  Enter select  r rename  d delete  Esc back"
    } else if app.tab().is_some_and(|t| t.doc.edit_mode) {
        "type hex/ASCII  Tab column  i overwrite/insert  Del delete  Ctrl+Z undo  Ctrl+S save  Esc view mode"
    } else {
        "i edit  v select  / search  n/N next  : jump  y copy  a add field  f fields  e endian  ? help  q quit"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))),
        area,
    );
}

fn unicode_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Navigation",
        &[
            ("Arrows / h j k l", "Move cursor"),
            ("Shift+Arrows", "Extend selection"),
            ("v", "Toggle visual selection (movement extends)"),
            ("PgUp / PgDn", "Move one page"),
            ("Home / End", "Start / end of row"),
            ("g / G, Ctrl+Home/End", "Start / end of file"),
            (": or Ctrl+G", "Jump to offset (dec or 0x hex)"),
            ("Mouse", "Click; drag to select (past edge scrolls); wheel"),
        ],
    ),
    (
        "Editing",
        &[
            ("i", "Cycle View -> Overwrite -> Insert -> View"),
            ("Insert", "Toggle Overwrite / Insert"),
            ("Esc", "Back to View Mode"),
            ("Tab", "Switch Hex / ASCII column"),
            ("Del / Backspace", "Delete selected bytes (edit mode)"),
            (
                "Paste (terminal)",
                "Paste hex or ASCII at cursor (edit mode)",
            ),
            ("u / Ctrl+Z", "Undo"),
            ("Ctrl+Y / Ctrl+R", "Redo"),
            ("Ctrl+S / S", "Save / Save as"),
        ],
    ),
    (
        "Search & clipboard",
        &[
            (
                "/ or Ctrl+F",
                "Search (hex, wildcards, text:, re:, u32le:, ...)",
            ),
            ("n / N, F3", "Next / previous match"),
            ("y or Ctrl+C", "Copy selection as hex (A1B2C3)"),
            ("Y", "Copy selection as spaced hex (A1 B2 C3)"),
            ("A", "Copy selection as ASCII"),
        ],
    ),
    (
        "Overlay fields",
        &[
            ("a", "Add field for selection"),
            ("[ / ]", "Previous / next field"),
            ("f", "Focus field list"),
            ("Space (list)", "Show / hide field"),
            ("b / t / c (list)", "Cycle border / text / background color"),
            ("r / d (list)", "Rename / delete field"),
        ],
    ),
    (
        "View & files",
        &[
            ("e", "Toggle inspector endianness"),
            ("p", "Toggle side panel"),
            ("+ / - / 0", "Bytes per row wider / narrower / fit"),
            ("o or Ctrl+O", "Open file"),
            ("Alt+Left/Right", "Previous / next file"),
            ("Ctrl+W", "Close file"),
            ("q or Ctrl+Q", "Quit"),
        ],
    ),
];

fn render_help(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let w = area.width.min(78);
    let h = area.height.saturating_sub(2).min(48);
    let popup = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let mut lines = Vec::new();
    for (section, keys) in HELP {
        lines.push(Line::from(Span::styled(
            *section,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
        for (k, desc) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<22}"), Style::new().fg(Color::Yellow)),
                Span::raw(*desc),
            ]));
        }
        lines.push(Line::from(""));
    }
    let max_scroll = (lines.len() as u16).saturating_sub(h.saturating_sub(2));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll.min(max_scroll), 0))
            .block(
                Block::bordered()
                    .title(" Keybindings ")
                    .title_bottom(Line::from(" ↑/↓ scroll · Esc close ").right_aligned()),
            ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::LaunchOptions;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn app_with(bytes: Vec<u8>) -> App {
        let mut app = App::new(LaunchOptions::default());
        app.truecolor = false;
        let mut doc = Document::from_bytes("t.bin".into(), bytes);
        doc.select_offset(0x30, false);
        app.tabs.push(Tab::new(doc));
        app
    }

    fn draw(app: &mut App, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        app.prepare_frame(Rect::new(0, 0, w, h));
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Screen cell of the high hex digit of byte `col` on hex-view row `row`.
    fn hex_cell(app: &App, buf: &Buffer, row: u16, col: usize) -> ratatui::buffer::Cell {
        let geo = app.hex_geo;
        let x = geo.area.x + HexGeometry::hex_x(geo.off_w, col);
        buf[(x, geo.area.y + row)].clone()
    }

    #[test]
    fn layout_fits_bytes_per_row_and_panel_to_the_terminal() {
        let mut app = app_with(vec![0; 256]);
        let wide = compute_layout(&app, Rect::new(0, 0, 130, 30));
        assert_eq!(wide.geometry.bpr, 16);
        assert!(wide.panel.is_some());

        let classic = compute_layout(&app, Rect::new(0, 0, 80, 24));
        assert_eq!(
            classic.geometry.bpr, 16,
            "80 columns keeps 16 bytes and drops the panel"
        );
        assert!(classic.panel.is_none());

        let narrow = compute_layout(&app, Rect::new(0, 0, 50, 24));
        assert_eq!(narrow.geometry.bpr, 8);

        let huge = compute_layout(&app, Rect::new(0, 0, 250, 30));
        assert_eq!(huge.geometry.bpr, 32);

        app.show_panel = false;
        assert!(
            compute_layout(&app, Rect::new(0, 0, 130, 30))
                .panel
                .is_none()
        );

        app.manual_bpr = Some(8);
        assert_eq!(
            compute_layout(&app, Rect::new(0, 0, 130, 30)).geometry.bpr,
            8
        );
    }

    #[test]
    fn rows_show_offsets_hex_and_ascii() {
        let mut app = app_with(b"Hello, world!\0\x01\x7f".to_vec());
        let buf = draw(&mut app, 100, 10);
        let geo = app.hex_geo;
        let row: String = (geo.area.x..geo.area.x + HexGeometry::width(geo.off_w, 16))
            .map(|x| buf[(x, geo.area.y)].symbol().to_string())
            .collect();
        assert!(
            row.starts_with("00000000  48 65 6C 6C 6F 2C 20 77  6F"),
            "{row}"
        );
        assert!(row.ends_with("Hello, world!..."), "{row}");
    }

    #[test]
    fn field_styles_map_to_terminal_attributes() {
        let mut app = app_with(vec![0x41; 64]);
        let doc = &mut app.tabs[0].doc;
        doc.add_field(Field {
            name: "border".into(),
            start: 0,
            end: 1,
            border: Some(FieldColor::Red),
            ..Field::default()
        });
        doc.add_field(Field {
            name: "styled".into(),
            start: 4,
            end: 5,
            border: None,
            text: Some(FieldColor::Green),
            background: Some(FieldColor::Blue),
            ..Field::default()
        });
        let buf = draw(&mut app, 120, 12);

        let border = hex_cell(&app, &buf, 0, 0);
        assert!(border.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(border.underline_color, Color::Red);
        // The gap between two bytes of one field continues the underline.
        let geo = app.hex_geo;
        let gap = &buf[(
            geo.area.x + HexGeometry::hex_x(geo.off_w, 0) + 2,
            geo.area.y,
        )];
        assert!(gap.modifier.contains(Modifier::UNDERLINED));

        let styled = hex_cell(&app, &buf, 0, 4);
        assert_eq!(styled.fg, Color::Green);
        assert_eq!(styled.bg, Color::Blue);
        assert!(!styled.modifier.contains(Modifier::UNDERLINED));

        let plain = hex_cell(&app, &buf, 0, 8);
        assert!(!plain.modifier.contains(Modifier::UNDERLINED));
        assert_eq!(plain.bg, Color::Reset);
    }

    #[test]
    fn hidden_fields_are_not_styled() {
        let mut app = app_with(vec![0x41; 64]);
        app.tabs[0].doc.add_field(Field {
            name: "f".into(),
            start: 0,
            end: 3,
            background: Some(FieldColor::Magenta),
            visible: false,
            ..Field::default()
        });
        let buf = draw(&mut app, 120, 12);
        let cell = hex_cell(&app, &buf, 0, 1);
        assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        assert_ne!(cell.bg, Color::Magenta);
    }

    #[test]
    fn selection_and_cursor_are_highlighted() {
        let mut app = app_with(vec![0; 64]);
        let doc = &mut app.tabs[0].doc;
        doc.select_offset(0x10, false);
        doc.select_offset(0x12, true);
        let buf = draw(&mut app, 120, 12);
        assert_eq!(hex_cell(&app, &buf, 1, 0).bg, Color::Blue);
        assert!(
            hex_cell(&app, &buf, 1, 2)
                .modifier
                .contains(Modifier::REVERSED),
            "cursor"
        );
        assert_eq!(hex_cell(&app, &buf, 1, 3).bg, Color::Reset);
    }

    #[test]
    fn rendering_never_panics_on_tiny_or_empty_screens() {
        for (w, h) in [(1, 1), (10, 3), (20, 5), (40, 8), (79, 24)] {
            let mut app = app_with(vec![0; 300]);
            app.show_help = true;
            draw(&mut app, w, h);
            app.show_help = false;
            app.tabs[0].doc.add_field(Field {
                name: "f".into(),
                end: 4,
                ..Field::default()
            });
            draw(&mut app, w, h);
            let mut empty = App::new(LaunchOptions::default());
            draw(&mut empty, w, h);
        }
    }

    #[test]
    fn welcome_screen_when_no_file_is_open() {
        let mut app = App::new(LaunchOptions::default());
        let buf = draw(&mut app, 60, 12);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("No file is open."));
    }
}
