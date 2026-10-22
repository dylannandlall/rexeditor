use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_BYTES_PER_ROW: usize = 16;
pub const OVERLAY_FORMAT_VERSION: u32 = 1;

/// UI-agnostic RGBA color representation `[r, g, b, a]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbaColor(pub u8, pub u8, pub u8, pub u8);

impl RgbaColor {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b, 255)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(r, g, b, a)
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self(self.0, self.1, self.2, a)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: usize,
    pub cursor: usize,
}

impl Selection {
    pub fn new(offset: usize) -> Self {
        Self {
            anchor: offset,
            cursor: offset,
        }
    }

    pub fn from_range(start: usize, end: usize) -> Self {
        Self {
            anchor: start,
            cursor: end,
        }
    }

    pub fn start(self) -> usize {
        self.anchor.min(self.cursor)
    }

    pub fn end(self) -> usize {
        self.anchor.max(self.cursor)
    }

    pub fn len(self) -> usize {
        self.end() - self.start() + 1
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn contains(self, offset: usize) -> bool {
        (self.start()..=self.end()).contains(&offset)
    }
}

/// Merges overlapping or adjacent selections into sorted non-overlapping ranges.
pub fn merge_selections(primary: Option<Selection>, additional: &[Selection]) -> Vec<Selection> {
    let mut ranges = additional.to_vec();
    if let Some(selection) = primary {
        ranges.push(selection);
    }
    ranges.sort_unstable_by_key(|range| range.start());
    let mut merged = Vec::<Selection>::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start() <= previous.end().saturating_add(1)
        {
            previous.cursor = previous.end().max(range.end());
        } else {
            merged.push(Selection {
                anchor: range.start(),
                cursor: range.end(),
            });
        }
    }
    merged
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Field {
    pub name: String,
    pub description: String,
    pub start: usize,
    pub end: usize,
    pub color: FieldColor,
    /// Background shading behind the field's bytes, independent of `color`.
    pub background: Option<FieldColor>,
}

impl Field {
    pub fn contains(&self, offset: usize) -> bool {
        (self.start..=self.end).contains(&offset)
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start) + 1
    }

    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FieldColor {
    #[default]
    Cyan,
    Green,
    Yellow,
    Magenta,
    Blue,
    Red,
    LightCyan,
    LightGreen,
    LightYellow,
    LightMagenta,
    LightBlue,
    LightRed,
    White,
    Gray,
}

impl FieldColor {
    pub const ALL: [Self; 14] = [
        Self::Cyan,
        Self::Green,
        Self::Yellow,
        Self::Magenta,
        Self::Blue,
        Self::Red,
        Self::LightCyan,
        Self::LightGreen,
        Self::LightYellow,
        Self::LightMagenta,
        Self::LightBlue,
        Self::LightRed,
        Self::White,
        Self::Gray,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Cyan => "cyan",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Magenta => "magenta",
            Self::Blue => "blue",
            Self::Red => "red",
            Self::LightCyan => "light cyan",
            Self::LightGreen => "light green",
            Self::LightYellow => "light yellow",
            Self::LightMagenta => "light magenta",
            Self::LightBlue => "light blue",
            Self::LightRed => "light red",
            Self::White => "white",
            Self::Gray => "gray",
        }
    }

    pub const fn rgba(self) -> RgbaColor {
        match self {
            Self::Cyan => RgbaColor::rgb(0, 188, 212),
            Self::Green => RgbaColor::rgb(76, 175, 80),
            Self::Yellow => RgbaColor::rgb(255, 193, 7),
            Self::Magenta => RgbaColor::rgb(186, 104, 200),
            Self::Blue => RgbaColor::rgb(66, 165, 245),
            Self::Red => RgbaColor::rgb(239, 83, 80),
            Self::LightCyan => RgbaColor::rgb(128, 222, 234),
            Self::LightGreen => RgbaColor::rgb(129, 199, 132),
            Self::LightYellow => RgbaColor::rgb(255, 241, 118),
            Self::LightMagenta => RgbaColor::rgb(225, 190, 231),
            Self::LightBlue => RgbaColor::rgb(144, 202, 249),
            Self::LightRed => RgbaColor::rgb(255, 138, 128),
            Self::White => RgbaColor::rgb(240, 244, 248),
            Self::Gray => RgbaColor::rgb(158, 158, 158),
        }
    }
}

/// Cycles a field's optional background color, inserting `None` between ends.
pub fn cycle_field_background(current: Option<FieldColor>, forward: bool) -> Option<FieldColor> {
    let last = *FieldColor::ALL
        .last()
        .expect("FieldColor::ALL is non-empty");
    let first = FieldColor::ALL[0];
    match (current, forward) {
        (None, true) => Some(first),
        (None, false) => Some(last),
        (Some(color), true) if color == last => None,
        (Some(color), false) if color == first => None,
        (Some(color), true) => Some(color.next()),
        (Some(color), false) => Some(color.previous()),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<OverlayMetadata>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OverlayMetadata {
    pub format_version: u32,
    pub file_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub content_id: String,
    pub created: String,
    pub updated: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub start: usize,
    pub end: usize,
}

impl SearchMatch {
    pub fn contains(&self, offset: usize) -> bool {
        (self.start..=self.end).contains(&offset)
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start) + 1
    }

    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ByteColorMode {
    #[default]
    Plain,
    Alternating,
    ByteClass,
    HighNibble,
    LowNibble,
    ZeroBytes,
    Printable,
    ValueBands,
}

impl ByteColorMode {
    pub const ALL: [Self; 8] = [
        Self::Plain,
        Self::Alternating,
        Self::ByteClass,
        Self::HighNibble,
        Self::LowNibble,
        Self::ZeroBytes,
        Self::Printable,
        Self::ValueBands,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[index.checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain white",
            Self::Alternating => "alternating bytes",
            Self::ByteClass => "byte classes",
            Self::HighNibble => "high-nibble bands",
            Self::LowNibble => "low-nibble bands",
            Self::ZeroBytes => "zero vs non-zero",
            Self::Printable => "printable vs binary",
            Self::ValueBands => "four value bands",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    DarkGray,
    Gray,
    #[default]
    White,
    Red,
    LightRed,
    Green,
    LightGreen,
    Yellow,
    LightYellow,
    Blue,
    LightBlue,
    Magenta,
    LightMagenta,
    Cyan,
    LightCyan,
}

impl NamedColor {
    pub const ALL: [Self; 16] = [
        Self::Black,
        Self::DarkGray,
        Self::Gray,
        Self::White,
        Self::Red,
        Self::LightRed,
        Self::Green,
        Self::LightGreen,
        Self::Yellow,
        Self::LightYellow,
        Self::Blue,
        Self::LightBlue,
        Self::Magenta,
        Self::LightMagenta,
        Self::Cyan,
        Self::LightCyan,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[index.checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::DarkGray => "dark gray",
            Self::Gray => "gray",
            Self::White => "white",
            Self::Red => "red",
            Self::LightRed => "light red",
            Self::Green => "green",
            Self::LightGreen => "light green",
            Self::Yellow => "yellow",
            Self::LightYellow => "light yellow",
            Self::Blue => "blue",
            Self::LightBlue => "light blue",
            Self::Magenta => "magenta",
            Self::LightMagenta => "light magenta",
            Self::Cyan => "cyan",
            Self::LightCyan => "light cyan",
        }
    }

    pub const fn rgba(self) -> RgbaColor {
        match self {
            Self::Black => RgbaColor::rgb(18, 20, 24),
            Self::DarkGray => RgbaColor::rgb(108, 117, 125),
            Self::Gray => RgbaColor::rgb(160, 168, 176),
            Self::White => RgbaColor::rgb(230, 235, 240),
            Self::Red => RgbaColor::rgb(220, 53, 69),
            Self::LightRed => RgbaColor::rgb(255, 115, 115),
            Self::Green => RgbaColor::rgb(46, 184, 92),
            Self::LightGreen => RgbaColor::rgb(120, 224, 143),
            Self::Yellow => RgbaColor::rgb(240, 180, 41),
            Self::LightYellow => RgbaColor::rgb(255, 224, 102),
            Self::Blue => RgbaColor::rgb(43, 114, 217),
            Self::LightBlue => RgbaColor::rgb(116, 185, 255),
            Self::Magenta => RgbaColor::rgb(175, 82, 222),
            Self::LightMagenta => RgbaColor::rgb(217, 140, 255),
            Self::Cyan => RgbaColor::rgb(23, 190, 207),
            Self::LightCyan => RgbaColor::rgb(114, 235, 245),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub byte_mode: ByteColorMode,
    pub hex_primary: NamedColor,
    pub hex_secondary: NamedColor,
    pub ascii: NamedColor,
    pub offset: NamedColor,
    pub border: NamedColor,
    pub selection_background: NamedColor,
    pub search_background: NamedColor,
    pub modified: NamedColor,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            byte_mode: ByteColorMode::Plain,
            hex_primary: NamedColor::White,
            hex_secondary: NamedColor::Cyan,
            ascii: NamedColor::White,
            offset: NamedColor::DarkGray,
            border: NamedColor::DarkGray,
            selection_background: NamedColor::Blue,
            search_background: NamedColor::Yellow,
            modified: NamedColor::LightRed,
        }
    }
}

impl Theme {
    pub fn color_for_byte(&self, offset: usize, byte: u8, ascii: bool) -> NamedColor {
        if matches!(self.byte_mode, ByteColorMode::Plain) {
            return self.hex_primary;
        }
        if ascii {
            return self.ascii;
        }
        match self.byte_mode {
            ByteColorMode::Plain => self.hex_primary,
            ByteColorMode::Alternating => {
                if offset.is_multiple_of(2) {
                    self.hex_primary
                } else {
                    self.hex_secondary
                }
            }
            ByteColorMode::ByteClass => {
                if byte == 0 {
                    self.offset
                } else if byte.is_ascii_graphic() || byte == b' ' {
                    self.hex_secondary
                } else if byte.is_ascii_control() {
                    self.modified
                } else {
                    self.hex_primary
                }
            }
            ByteColorMode::HighNibble => {
                if byte >> 4 < 8 {
                    self.hex_primary
                } else {
                    self.hex_secondary
                }
            }
            ByteColorMode::LowNibble => {
                if byte & 0x0F < 8 {
                    self.hex_primary
                } else {
                    self.hex_secondary
                }
            }
            ByteColorMode::ZeroBytes => {
                if byte == 0 {
                    self.hex_secondary
                } else {
                    self.hex_primary
                }
            }
            ByteColorMode::Printable => {
                if byte.is_ascii_graphic() || byte == b' ' {
                    self.hex_secondary
                } else {
                    self.hex_primary
                }
            }
            ByteColorMode::ValueBands => match byte >> 6 {
                0 => self.offset,
                1 => self.hex_secondary,
                2 => self.hex_primary,
                _ => self.modified,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ViewerSettings {
    pub show_ascii: bool,
    pub bytes_per_row: usize,
    pub uppercase_hex: bool,
    pub show_offsets: bool,
    pub show_sidebar: bool,
    pub compress_repeated_rows: bool,
    pub show_overlays: bool,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            show_ascii: true,
            bytes_per_row: DEFAULT_BYTES_PER_ROW,
            uppercase_hex: true,
            show_offsets: true,
            show_sidebar: true,
            compress_repeated_rows: false,
            show_overlays: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Bytes {
        offset: usize,
    },
    Repeated {
        start: usize,
        end: usize,
        byte: u8,
        physical_rows: usize,
    },
}

impl DisplayRow {
    pub fn start(self) -> usize {
        match self {
            Self::Bytes { offset } => offset,
            Self::Repeated { start, .. } => start,
        }
    }

    pub fn end(self, bytes_per_row: usize, byte_len: usize) -> usize {
        match self {
            Self::Bytes { offset } => offset
                .saturating_add(bytes_per_row)
                .min(byte_len)
                .saturating_sub(1),
            Self::Repeated { end, .. } => end,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    #[default]
    Overwrite,
    Insert,
}

impl EditKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Overwrite => "Overwrite",
            Self::Insert => "Insert",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Overwrite => Self::Insert,
            Self::Insert => Self::Overwrite,
        }
    }
}

/// Returns the default user data directory for `rexeditor` (compatible with `rexedit` subdirs).
pub fn rexedit_data_dir(subdir: &str) -> PathBuf {
    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA")
        && !appdata.trim().is_empty()
    {
        return PathBuf::from(appdata).join("rexedit").join(subdir);
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.trim().is_empty()
    {
        return PathBuf::from(xdg).join("rexedit").join(subdir);
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        && !home.trim().is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("rexedit")
            .join(subdir);
    }
    std::env::temp_dir().join("rexedit").join(subdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_colors_cycle_in_both_directions() {
        assert_eq!(FieldColor::Cyan.next(), FieldColor::Green);
        assert_eq!(FieldColor::Cyan.previous(), FieldColor::Gray);
        assert_eq!(FieldColor::Gray.next(), FieldColor::Cyan);
    }

    #[test]
    fn field_background_cycles_through_none_at_both_ends() {
        assert_eq!(cycle_field_background(None, true), Some(FieldColor::Cyan));
        assert_eq!(cycle_field_background(Some(FieldColor::Gray), true), None);
        assert_eq!(cycle_field_background(None, false), Some(FieldColor::Gray));
        assert_eq!(cycle_field_background(Some(FieldColor::Cyan), false), None);
    }

    #[test]
    fn merges_overlapping_and_adjacent_selections() {
        let merged = merge_selections(
            Some(Selection::from_range(4, 8)),
            &[Selection::from_range(0, 3), Selection::from_range(12, 15)],
        );
        assert_eq!(
            merged,
            vec![Selection::from_range(0, 8), Selection::from_range(12, 15)]
        );
    }
}
