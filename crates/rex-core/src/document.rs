use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    buffer::ByteBuffer,
    model::{
        DisplayRow, EditKind, Field, OVERLAY_FORMAT_VERSION, Overlay, OverlayMetadata, Selection,
        Theme, ViewerSettings, merge_selections, rexedit_data_dir,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditAction {
    Overwrite {
        offset: usize,
        before: Vec<u8>,
        after: Vec<u8>,
    },
    Insert {
        offset: usize,
        bytes: Vec<u8>,
    },
    Delete {
        offset: usize,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    HexSpaced,
    HexCompact,
    Ascii,
    CArray,
    RustArray,
    PythonBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingEdit {
    Overwrite { offset: usize, before: u8 },
    Insert { offset: usize, high_nibble: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollapsedRun {
    phys_start: usize,
    phys_len: usize,
    display_row: usize,
    hidden_before: usize,
    byte: u8,
}

pub struct Document {
    pub path: PathBuf,
    pub buffer: ByteBuffer,
    pub scroll: usize,
    pub scroll_requested: bool,
    pub visible_rows: usize,
    pub selection: Option<Selection>,
    pub additional_selections: Vec<Selection>,
    pub fields: Vec<Field>,
    pub selected_field: usize,
    pub theme: Theme,
    pub settings: ViewerSettings,
    pub edit_mode: bool,
    pub edit_kind: EditKind,
    pub edit_high_nibble: bool,
    pub insert_at_end: bool,
    pub status: String,
    undo_stack: Vec<EditAction>,
    redo_stack: Vec<EditAction>,
    pending_edit: Option<PendingEdit>,
    saved_content_id: String,
    overlay_dir: PathBuf,
    overlay_path: Option<PathBuf>,
    overlay_origin: Option<OverlayMetadata>,
    overlay_foreign: bool,
    backed_up_overlays: HashSet<PathBuf>,
    collapsed_runs: Vec<CollapsedRun>,
}

impl Document {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let buffer = ByteBuffer::open(&path)?;
        Ok(Self::from_buffer(path, buffer, overlay_storage_dir()))
    }

    pub fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> Self {
        Self::from_buffer(path, ByteBuffer::from_bytes(bytes), overlay_storage_dir())
    }

    pub fn with_overlay_dir(path: PathBuf, bytes: Vec<u8>, overlay_dir: PathBuf) -> Self {
        Self::from_buffer(path, ByteBuffer::from_bytes(bytes), overlay_dir)
    }

    fn from_buffer(path: PathBuf, buffer: ByteBuffer, overlay_dir: PathBuf) -> Self {
        let selection = (!buffer.is_empty()).then(|| Selection::new(0));
        let saved_content_id = buffer.content_identity();
        let mut doc = Self {
            path,
            buffer,
            scroll: 0,
            scroll_requested: false,
            visible_rows: 1,
            selection,
            additional_selections: Vec::new(),
            fields: Vec::new(),
            selected_field: 0,
            theme: Theme::default(),
            settings: ViewerSettings::default(),
            edit_mode: false,
            edit_kind: EditKind::Overwrite,
            edit_high_nibble: true,
            insert_at_end: false,
            status: "Ready".into(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_edit: None,
            saved_content_id,
            overlay_dir,
            overlay_path: None,
            overlay_origin: None,
            overlay_foreign: false,
            backed_up_overlays: HashSet::new(),
            collapsed_runs: Vec::new(),
        };
        if doc.path.is_file() {
            match doc.restore_automatic_overlay() {
                Ok(Some(overlay_path)) => {
                    doc.status = format!(
                        "Restored {} saved overlay field(s) from {}",
                        doc.fields.len(),
                        overlay_path.display()
                    );
                }
                Ok(None) => {}
                Err(error) => doc.status = format!("Could not restore saved overlay: {error}"),
            }
        }
        doc.rebuild_display_rows();
        doc
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn row_count(&self) -> usize {
        let bytes_per_row = self.settings.bytes_per_row.max(1);
        let physical_rows = self.buffer.len().div_ceil(bytes_per_row);
        if self.settings.compress_repeated_rows {
            let hidden = self
                .collapsed_runs
                .last()
                .map_or(0, |last| last.hidden_before + last.phys_len - 1);
            physical_rows.saturating_sub(hidden)
        } else {
            physical_rows
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.visible_rows)
    }

    pub fn display_row(&self, row: usize) -> DisplayRow {
        let bytes_per_row = self.settings.bytes_per_row.max(1);
        if self.settings.compress_repeated_rows && !self.collapsed_runs.is_empty() {
            let idx = self
                .collapsed_runs
                .partition_point(|run| run.display_row < row);
            if let Some(run) = self.collapsed_runs.get(idx)
                && run.display_row == row
            {
                return DisplayRow::Repeated {
                    start: run.phys_start * bytes_per_row,
                    end: (run.phys_start + run.phys_len) * bytes_per_row - 1,
                    byte: run.byte,
                    physical_rows: run.phys_len,
                };
            }
            let hidden = if idx == 0 {
                0
            } else {
                let prev = self.collapsed_runs[idx - 1];
                prev.hidden_before + prev.phys_len - 1
            };
            let phys_row = row.saturating_add(hidden);
            DisplayRow::Bytes {
                offset: phys_row.saturating_mul(bytes_per_row),
            }
        } else {
            DisplayRow::Bytes {
                offset: row.saturating_mul(bytes_per_row),
            }
        }
    }

    pub fn display_row_for_offset(&self, offset: usize) -> usize {
        let rows = self.row_count();
        if rows == 0 {
            return 0;
        }
        let bytes_per_row = self.settings.bytes_per_row.max(1);
        let physical_rows = self.buffer.len().div_ceil(bytes_per_row);
        let phys_row = (offset / bytes_per_row).min(physical_rows.saturating_sub(1));
        if self.settings.compress_repeated_rows && !self.collapsed_runs.is_empty() {
            let idx = self
                .collapsed_runs
                .partition_point(|run| run.phys_start + run.phys_len <= phys_row);
            if let Some(run) = self.collapsed_runs.get(idx)
                && phys_row >= run.phys_start
            {
                return run.display_row;
            }
            let hidden = if idx == 0 {
                0
            } else {
                let prev = self.collapsed_runs[idx - 1];
                prev.hidden_before + prev.phys_len - 1
            };
            phys_row.saturating_sub(hidden).min(rows - 1)
        } else {
            phys_row.min(rows - 1)
        }
    }

    pub fn rebuild_display_rows(&mut self) {
        self.collapsed_runs.clear();
        if !self.settings.compress_repeated_rows {
            self.scroll = self.scroll.min(self.max_scroll());
            return;
        }
        let bytes_per_row = self.settings.bytes_per_row.max(1);
        let len = self.buffer.len();
        let full_rows = len / bytes_per_row;
        let mut row = 0;
        let mut hidden_so_far = 0;
        if let Some(slice) = self.buffer.as_contiguous_slice() {
            while row < full_rows {
                let offset = row * bytes_per_row;
                let first_row = &slice[offset..offset + bytes_per_row];
                let byte = first_row[0];
                if first_row.iter().all(|&candidate| candidate == byte) {
                    let mut run_end = row + 1;
                    while run_end < full_rows {
                        let next = run_end * bytes_per_row;
                        let next_row = &slice[next..next + bytes_per_row];
                        if !next_row.iter().all(|&candidate| candidate == byte) {
                            break;
                        }
                        run_end += 1;
                    }
                    let run_rows = run_end - row;
                    if run_rows >= 3 {
                        self.collapsed_runs.push(CollapsedRun {
                            phys_start: row,
                            phys_len: run_rows,
                            display_row: row - hidden_so_far,
                            hidden_before: hidden_so_far,
                            byte,
                        });
                        hidden_so_far += run_rows - 1;
                    }
                    row = run_end;
                } else {
                    row += 1;
                }
            }
        } else {
            let mut row_buf = vec![0u8; bytes_per_row];
            while row < full_rows {
                let offset = row * bytes_per_row;
                self.buffer.copy_range(offset, &mut row_buf);
                let byte = row_buf[0];
                if row_buf.iter().all(|&candidate| candidate == byte) {
                    let mut run_end = row + 1;
                    while run_end < full_rows {
                        let next = run_end * bytes_per_row;
                        self.buffer.copy_range(next, &mut row_buf);
                        if !row_buf.iter().all(|&candidate| candidate == byte) {
                            break;
                        }
                        run_end += 1;
                    }
                    let run_rows = run_end - row;
                    if run_rows >= 3 {
                        self.collapsed_runs.push(CollapsedRun {
                            phys_start: row,
                            phys_len: run_rows,
                            display_row: row - hidden_so_far,
                            hidden_before: hidden_so_far,
                            byte,
                        });
                        hidden_so_far += run_rows - 1;
                    }
                    row = run_end;
                } else {
                    row += 1;
                }
            }
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn select_offset(&mut self, offset: usize, extend: bool) {
        if self.buffer.is_empty() {
            return;
        }
        let offset = offset.min(self.buffer.len() - 1);
        if extend {
            if let Some(selection) = &mut self.selection {
                selection.cursor = offset;
            } else {
                self.selection = Some(Selection::new(offset));
            }
        } else {
            self.selection = Some(Selection::new(offset));
            self.additional_selections.clear();
        }
        self.edit_high_nibble = true;
        self.insert_at_end = false;
        self.ensure_visible(offset);
    }

    pub fn add_selection(&mut self, offset: usize) {
        if self.buffer.is_empty() {
            return;
        }
        let offset = offset.min(self.buffer.len() - 1);
        if let Some(selection) = self.selection.take() {
            self.additional_selections.push(selection);
        }
        self.selection = Some(Selection::new(offset));
        self.edit_high_nibble = true;
        self.insert_at_end = false;
    }

    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.buffer.is_empty() {
            return;
        }
        let current = self.selection.map_or(0, |selection| selection.cursor);
        let offset = current
            .saturating_add_signed(delta)
            .min(self.buffer.len() - 1);
        self.select_offset(offset, extend);
        self.edit_high_nibble = true;
    }

    pub fn ensure_visible(&mut self, offset: usize) {
        let row = self.display_row_for_offset(offset);
        let prev = self.scroll;
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll.saturating_add(self.visible_rows) {
            self.scroll = row.saturating_sub(self.visible_rows.saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.max_scroll());
        if self.scroll != prev {
            self.scroll_requested = true;
        }
    }

    pub fn selected_ranges(&self) -> Vec<Selection> {
        merge_selections(self.selection, &self.additional_selections)
    }

    pub fn selected_bytes(&self) -> Vec<u8> {
        let Some(last) = self.buffer.len().checked_sub(1) else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        for range in self.selected_ranges() {
            let start = range.start().min(last);
            let end = range.end().min(last);
            bytes.extend(self.buffer.read_range(start, end - start + 1));
        }
        bytes
    }

    pub fn current_bytes(&self, max_len: usize) -> Vec<u8> {
        let Some(selection) = self.selection else {
            return Vec::new();
        };
        let Some(last) = self.buffer.len().checked_sub(1) else {
            return Vec::new();
        };
        let start = selection.start().min(last);
        let end = selection.end().min(last);
        let len = (end - start + 1).min(max_len);
        self.buffer.read_range(start, len)
    }

    /// Reads up to `max_len` bytes starting at `selection.start()` (even if only 1 byte is selected)
    /// so the Data Inspector can decode multi-byte integers and floats at the cursor position.
    pub fn inspector_window_bytes(&self, max_len: usize) -> Vec<u8> {
        let Some(selection) = self.selection else {
            return Vec::new();
        };
        let Some(last) = self.buffer.len().checked_sub(1) else {
            return Vec::new();
        };
        let start = selection.start().min(last);
        let len = (self.buffer.len() - start).min(max_len);
        self.buffer.read_range(start, len)
    }

    pub fn pending_insert_nibble(&self) -> Option<(usize, u8)> {
        if let Some(PendingEdit::Insert {
            offset,
            high_nibble,
        }) = self.pending_edit
        {
            Some((offset, high_nibble))
        } else {
            None
        }
    }

    pub fn format_selected_bytes(&self, format: CopyFormat) -> Option<String> {
        let bytes = self.selected_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(match format {
            CopyFormat::HexSpaced => bytes
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" "),
            CopyFormat::HexCompact => bytes.iter().map(|b| format!("{b:02X}")).collect(),
            CopyFormat::Ascii => bytes
                .iter()
                .map(|&b| {
                    if (0x20..=0x7E).contains(&b) {
                        char::from(b)
                    } else {
                        '.'
                    }
                })
                .collect(),
            CopyFormat::CArray => {
                let inner = bytes
                    .iter()
                    .map(|b| format!("0x{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {inner} }}")
            }
            CopyFormat::RustArray => {
                let inner = bytes
                    .iter()
                    .map(|b| format!("0x{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{inner}]")
            }
            CopyFormat::PythonBytes => {
                let inner: String = bytes.iter().map(|b| format!("\\x{b:02x}")).collect();
                format!("b\"{inner}\"")
            }
        })
    }

    pub fn is_selected(&self, offset: usize) -> bool {
        self.selection
            .is_some_and(|selection| selection.contains(offset))
            || self
                .additional_selections
                .iter()
                .any(|selection| selection.contains(offset))
    }

    pub fn cycle_edit_mode(&mut self) {
        self.commit_pending_edit();
        if !self.edit_mode {
            self.edit_mode = true;
            self.edit_kind = EditKind::Overwrite;
            self.edit_high_nibble = true;
            self.insert_at_end = false;
            self.status = "Overwrite Mode (press 'i' for Insert Mode, Esc for View Mode)".into();
        } else if self.edit_kind == EditKind::Overwrite {
            self.edit_kind = EditKind::Insert;
            self.edit_high_nibble = true;
            self.status = "Insert Mode (press 'i' or Esc for View Mode)".into();
        } else {
            self.edit_mode = false;
            self.edit_kind = EditKind::Overwrite;
            self.edit_high_nibble = true;
            self.insert_at_end = false;
            self.status = "View Mode".into();
        }
    }

    pub fn edit_nibble(&mut self, nibble: u8) {
        let nibble = nibble & 0x0F;
        match self.edit_kind {
            EditKind::Overwrite => self.overwrite_nibble(nibble),
            EditKind::Insert => self.insert_nibble(nibble),
        }
    }

    fn overwrite_nibble(&mut self, nibble: u8) {
        let Some(offset) = self.selection.map(|selection| selection.cursor) else {
            return;
        };
        let Some(original) = self.buffer.get(offset) else {
            return;
        };
        if self.edit_high_nibble {
            self.pending_edit = Some(PendingEdit::Overwrite {
                offset,
                before: original,
            });
        }
        let updated = if self.edit_high_nibble {
            (nibble << 4) | (original & 0x0F)
        } else {
            (original & 0xF0) | nibble
        };
        self.buffer.overwrite(offset, &[updated]);
        self.rebuild_display_rows();
        if self.edit_high_nibble {
            self.edit_high_nibble = false;
        } else {
            self.commit_pending_edit();
            let next = (offset + 1).min(self.buffer.len().saturating_sub(1));
            self.selection = Some(Selection::new(next));
            self.additional_selections.clear();
            self.ensure_visible(next);
        }
        self.status = format!("Modified byte at 0x{offset:X}; Ctrl+S saves");
    }

    fn insert_nibble(&mut self, nibble: u8) {
        let offset = if self.insert_at_end {
            self.buffer.len()
        } else {
            self.selection
                .map(|selection| selection.cursor)
                .unwrap_or(self.buffer.len())
        };
        if self.edit_high_nibble {
            self.pending_edit = Some(PendingEdit::Insert {
                offset,
                high_nibble: nibble,
            });
            self.edit_high_nibble = false;
            self.status = format!("Insert at 0x{offset:X}: enter the second hex digit");
            return;
        }
        let Some(PendingEdit::Insert {
            offset,
            high_nibble,
        }) = self.pending_edit.take()
        else {
            self.edit_high_nibble = true;
            return;
        };
        let byte = (high_nibble << 4) | nibble;
        self.insert_bytes_at(offset, &[byte]);
        self.undo_stack.push(EditAction::Insert {
            offset,
            bytes: vec![byte],
        });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + 1).min(self.buffer.len().saturating_sub(1));
        self.selection = Some(Selection::new(next));
        self.insert_at_end = offset + 1 == self.buffer.len();
        self.ensure_visible(next);
        self.status = format!("Inserted {byte:02X} at 0x{offset:X}; Ctrl+S saves");
    }

    pub fn edit_ascii_char(&mut self, ch: char) {
        if !ch.is_ascii() {
            return;
        }
        self.commit_pending_edit();
        let byte = ch as u8;
        match self.edit_kind {
            EditKind::Overwrite => {
                if let Some(offset) = self.selection.map(|selection| selection.cursor) {
                    self.overwrite_bytes(offset, &[byte]);
                }
            }
            EditKind::Insert => {
                let offset = if self.insert_at_end {
                    self.buffer.len()
                } else {
                    self.selection
                        .map(|selection| selection.cursor)
                        .unwrap_or(self.buffer.len())
                };
                self.insert_bytes(offset, &[byte]);
            }
        }
    }

    pub fn overwrite_bytes(&mut self, offset: usize, data: &[u8]) {
        self.commit_pending_edit();
        if offset >= self.buffer.len() || data.is_empty() {
            self.status = "No bytes selected to overwrite".into();
            return;
        }
        let available = self.buffer.len() - offset;
        let truncated = data.len() > available;
        let bytes = &data[..data.len().min(available)];
        let before = self.buffer.overwrite(offset, bytes);
        self.rebuild_display_rows();
        self.undo_stack.push(EditAction::Overwrite {
            offset,
            before,
            after: bytes.to_vec(),
        });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + bytes.len()).min(self.buffer.len() - 1);
        self.selection = Some(Selection::new(next));
        self.ensure_visible(next);
        self.status = if truncated {
            format!(
                "Overwrote {} byte(s) at 0x{offset:X} (truncated at end of file); Ctrl+S saves",
                bytes.len()
            )
        } else {
            format!(
                "Overwrote {} byte(s) at 0x{offset:X}; Ctrl+S saves",
                bytes.len()
            )
        };
    }

    pub fn insert_bytes(&mut self, offset: usize, data: &[u8]) {
        self.commit_pending_edit();
        if data.is_empty() {
            return;
        }
        let offset = offset.min(self.buffer.len());
        self.insert_bytes_at(offset, data);
        self.undo_stack.push(EditAction::Insert {
            offset,
            bytes: data.to_vec(),
        });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + data.len()).min(self.buffer.len().saturating_sub(1));
        self.selection = Some(Selection::new(next));
        self.insert_at_end = offset + data.len() == self.buffer.len();
        self.ensure_visible(next);
        self.status = format!(
            "Inserted {} byte(s) at 0x{offset:X}; Ctrl+S saves",
            data.len()
        );
    }

    pub fn delete_selected_bytes(&mut self) {
        self.commit_pending_edit();
        let Some(selection) = self.selection else {
            self.status = "No bytes selected to delete".into();
            return;
        };
        if self.buffer.is_empty() {
            self.status = "No bytes selected to delete".into();
            return;
        }
        let start = selection.start().min(self.buffer.len() - 1);
        let end = selection.end().min(self.buffer.len() - 1);
        let removed = self.delete_bytes_at(start, end - start + 1);
        if removed.is_empty() {
            self.status = "No bytes selected to delete".into();
            return;
        }
        self.undo_stack.push(EditAction::Delete {
            offset: start,
            bytes: removed.clone(),
        });
        self.redo_stack.clear();
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.buffer.is_empty() {
            self.selection = None;
        } else {
            let next = start.min(self.buffer.len() - 1);
            self.selection = Some(Selection::new(next));
            self.ensure_visible(next);
        }
        self.status = format!("Deleted {} byte(s); Ctrl+S saves", removed.len());
    }

    pub fn paste_hex_bytes(&mut self, text: &str) {
        self.commit_pending_edit();
        let trimmed = text.trim();
        let body = trimmed
            .strip_prefix("b\"")
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| trimmed.strip_prefix("b'").and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(trimmed);
        let cleaned = body
            .replace("0x", " ")
            .replace("0X", " ")
            .replace("\\x", " ")
            .replace("\\X", " ");
        let mut bytes = Vec::new();
        let mut pending_high_nibble = None;
        for character in cleaned.chars() {
            if !character.is_ascii_hexdigit() {
                continue;
            }
            let nibble = character.to_digit(16).expect("checked hex digit") as u8;
            match pending_high_nibble.take() {
                Some(high) => bytes.push((high << 4) | nibble),
                None => pending_high_nibble = Some(nibble),
            }
        }
        if bytes.is_empty() {
            self.status = "Clipboard did not contain a full byte of hex digits".into();
            return;
        }
        match self.edit_kind {
            EditKind::Insert => {
                let offset = if self.insert_at_end {
                    self.buffer.len()
                } else {
                    self.selection
                        .map(|selection| selection.cursor)
                        .unwrap_or(self.buffer.len())
                };
                self.insert_bytes(offset, &bytes);
            }
            EditKind::Overwrite => {
                if let Some(offset) = self.selection.map(|selection| selection.cursor) {
                    self.overwrite_bytes(offset, &bytes);
                } else {
                    self.status = "No bytes selected to overwrite".into();
                }
            }
        }
        if pending_high_nibble.is_some() {
            self.status = format!("{} (trailing hex digit ignored)", self.status);
        }
    }

    pub fn paste_ascii_bytes(&mut self, text: &str) {
        self.commit_pending_edit();
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            self.status = "Clipboard is empty".into();
            return;
        }
        match self.edit_kind {
            EditKind::Insert => {
                let offset = if self.insert_at_end {
                    self.buffer.len()
                } else {
                    self.selection
                        .map(|selection| selection.cursor)
                        .unwrap_or(self.buffer.len())
                };
                self.insert_bytes(offset, bytes);
            }
            EditKind::Overwrite => {
                if let Some(offset) = self.selection.map(|selection| selection.cursor) {
                    self.overwrite_bytes(offset, bytes);
                } else {
                    self.status = "No bytes selected to overwrite".into();
                }
            }
        }
    }

    pub fn commit_pending_edit(&mut self) {
        let Some(pending) = self.pending_edit.take() else {
            return;
        };
        if let PendingEdit::Overwrite { offset, before } = pending
            && let Some(after) = self.buffer.get(offset)
            && before != after
        {
            self.undo_stack.push(EditAction::Overwrite {
                offset,
                before: vec![before],
                after: vec![after],
            });
            self.redo_stack.clear();
        }
        self.edit_high_nibble = true;
    }

    pub fn undo(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.undo_stack.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        let (offset, description) = match &action {
            EditAction::Overwrite { offset, before, .. } => {
                self.buffer.overwrite(*offset, before);
                self.rebuild_display_rows();
                (*offset, "overwrite")
            }
            EditAction::Insert { offset, bytes } => {
                self.delete_bytes_at(*offset, bytes.len());
                (*offset, "insertion")
            }
            EditAction::Delete { offset, bytes } => {
                self.insert_bytes_at(*offset, bytes);
                (*offset, "deletion")
            }
        };
        self.redo_stack.push(action);
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.buffer.is_empty() {
            self.selection = None;
        } else {
            let selection = offset.min(self.buffer.len() - 1);
            self.selection = Some(Selection::new(selection));
            self.ensure_visible(selection);
        }
        self.status = format!("Undid {description} at 0x{offset:X}");
    }

    pub fn redo(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.redo_stack.pop() else {
            self.status = "Nothing to redo".into();
            return;
        };
        let (offset, description) = match &action {
            EditAction::Overwrite { offset, after, .. } => {
                self.buffer.overwrite(*offset, after);
                self.rebuild_display_rows();
                (*offset, "overwrite")
            }
            EditAction::Insert { offset, bytes } => {
                self.insert_bytes_at(*offset, bytes);
                (*offset, "insertion")
            }
            EditAction::Delete { offset, bytes } => {
                self.delete_bytes_at(*offset, bytes.len());
                (*offset, "deletion")
            }
        };
        self.undo_stack.push(action);
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.buffer.is_empty() {
            self.selection = None;
        } else {
            let selection = offset.min(self.buffer.len() - 1);
            self.selection = Some(Selection::new(selection));
            self.ensure_visible(selection);
        }
        self.status = format!("Redid {description} at 0x{offset:X}");
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
            || matches!(self.pending_edit, Some(PendingEdit::Overwrite { .. }))
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    pub fn save(&mut self) -> Result<(), String> {
        let path = self.path.clone();
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: PathBuf) -> Result<(), String> {
        self.commit_pending_edit();
        let previous_file = fs::canonicalize(&self.path).ok();
        let previous_name = self.file_name();
        self.buffer
            .save_to_path(&path)
            .map_err(|error| format!("Could not save binary to {}: {error}", path.display()))?;
        let new_file = fs::canonicalize(&path).ok();
        let new_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let saved_elsewhere =
            previous_file.is_none() || previous_file != new_file || previous_name != new_name;
        self.path = path.clone();
        self.saved_content_id = self.buffer.content_identity();
        self.undo_stack.clear();
        self.redo_stack.clear();

        let split_from_original =
            saved_elsewhere && (self.overlay_path.is_some() || !self.fields.is_empty());
        if saved_elsewhere {
            self.overlay_path = None;
            self.overlay_origin = None;
            self.overlay_foreign = false;
        } else {
            self.overlay_foreign = false;
        }
        self.status = match self.persist_automatic_overlay() {
            Ok(Some(overlay)) => {
                let mut message = format!(
                    "Saved binary to {}; overlay saved to {}",
                    path.display(),
                    overlay.display()
                );
                if split_from_original {
                    message.push_str(&format!(" ({previous_name} keeps its own overlay)"));
                }
                message
            }
            Ok(None) => format!("Saved binary to {}", path.display()),
            Err(error) => format!("Saved binary to {}; {error}", path.display()),
        };
        Ok(())
    }

    pub fn add_field(&mut self, field: Field) {
        self.fields.push(field.clone());
        self.selected_field = self.fields.len() - 1;
        self.sort_fields();
        if let Some(index) = self.fields.iter().position(|candidate| *candidate == field) {
            self.selected_field = index;
        }
        self.status = "Field added".into();
        self.save_automatic_overlay_after_change();
    }

    pub fn update_field(&mut self, index: usize, field: Field) {
        if index >= self.fields.len() {
            return;
        }
        self.fields[index] = field.clone();
        self.selected_field = index;
        self.sort_fields();
        if let Some(new_index) = self.fields.iter().position(|candidate| *candidate == field) {
            self.selected_field = new_index;
        }
        self.status = "Field updated".into();
        self.save_automatic_overlay_after_change();
    }

    pub fn delete_field(&mut self, index: usize) {
        if index >= self.fields.len() {
            return;
        }
        let removed = self.fields.remove(index);
        self.selected_field = self.selected_field.min(self.fields.len().saturating_sub(1));
        self.status = format!("Deleted field '{}'", removed.name);
        self.save_automatic_overlay_after_change();
    }

    pub fn open_overlay_name(&self) -> Option<String> {
        self.overlay_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    }

    pub fn suggested_overlay_path(&self) -> PathBuf {
        self.overlay_path
            .clone()
            .unwrap_or_else(|| next_free_overlay_path(&self.overlay_dir, &self.file_name()))
    }

    pub fn save_overlay_to(&mut self, path: &Path) -> Result<String, String> {
        if self.overlay_path.as_deref() == Some(path) {
            return self.write_overlay(path);
        }
        let previous = (
            self.overlay_path.take(),
            self.overlay_origin.take(),
            self.overlay_foreign,
        );
        self.open_overlay(path.to_owned(), None);
        match self.write_overlay(path) {
            Ok(message) => {
                let full_message = format!("{message}; autosave now writes there");
                self.status = full_message.clone();
                Ok(full_message)
            }
            Err(error) => {
                (self.overlay_path, self.overlay_origin, self.overlay_foreign) = previous;
                Err(error)
            }
        }
    }

    pub fn load_overlay_from(&mut self, path: &Path) -> Result<String, String> {
        let overlay = read_overlay(path)?;
        self.apply_overlay_fields(overlay.fields);
        self.open_overlay(path.to_owned(), overlay.metadata);
        let mut message = format!(
            "Loaded {} fields from {}; autosave now writes there",
            self.fields.len(),
            path.display()
        );
        if self.overlay_foreign
            && let Some(origin) = &self.overlay_origin
        {
            message.push_str(&format!(
                " (made for a different binary: {})",
                origin.file_name
            ));
        }
        self.status = message.clone();
        Ok(message)
    }

    pub fn persist_automatic_overlay(&mut self) -> Result<Option<PathBuf>, String> {
        if !self.path.is_file() {
            return Ok(None);
        }
        if let Some(existing_path) = &self.overlay_path {
            let expected_prefix = format!("{}-overlay-", overlay_name_stem(&self.file_name()));
            let in_overlay_dir = existing_path.starts_with(&self.overlay_dir);
            let wrong_auto_name = in_overlay_dir
                && existing_path
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with(&expected_prefix));
            let wrong_origin = self.overlay_origin.as_ref().is_some_and(|origin| {
                (!origin.file_name.is_empty() && origin.file_name != self.file_name())
                    || (!origin.file_path.is_empty() && origin.file_path != self.canonical_path())
            });
            if wrong_auto_name || wrong_origin {
                self.overlay_path = None;
                self.overlay_origin = None;
                self.overlay_foreign = false;
            }
        }
        let path = match &self.overlay_path {
            Some(path) => path.clone(),
            None if self.fields.is_empty() => return Ok(None),
            None => {
                let path = allocate_overlay_path(&self.overlay_dir, &self.file_name()).map_err(
                    |error| {
                        format!(
                            "Could not create an overlay in {}: {error}",
                            self.overlay_dir.display()
                        )
                    },
                )?;
                self.backed_up_overlays.insert(path.clone());
                self.open_overlay(path.clone(), None);
                if let Err(error) = self.write_overlay(&path) {
                    let _ = fs::remove_file(&path);
                    self.overlay_path = None;
                    return Err(error);
                }
                return Ok(Some(path));
            }
        };
        self.write_overlay(&path)?;
        Ok(Some(path))
    }

    pub fn parse_offset(input: &str) -> Result<usize, String> {
        parse_offset(input)
    }

    fn insert_bytes_at(&mut self, offset: usize, bytes_to_insert: &[u8]) {
        if bytes_to_insert.is_empty() {
            return;
        }
        let offset = offset.min(self.buffer.len());
        self.buffer.insert(offset, bytes_to_insert);
        self.shift_fields_for_insert(offset, bytes_to_insert.len());
        self.sort_fields();
        self.rebuild_display_rows();
    }

    fn delete_bytes_at(&mut self, offset: usize, length: usize) -> Vec<u8> {
        if length == 0 || offset >= self.buffer.len() {
            return Vec::new();
        }
        let removed = self.buffer.delete(offset, length);
        self.shift_fields_for_delete(offset, removed.len());
        self.sort_fields();
        self.rebuild_display_rows();
        removed
    }

    fn sort_fields(&mut self) {
        if self
            .fields
            .is_sorted_by(|a, b| field_order(a, b) != std::cmp::Ordering::Greater)
        {
            return;
        }
        let selected = self.fields.get(self.selected_field).cloned();
        self.fields.sort_by(field_order);
        if let Some(selected) = selected
            && let Some(index) = self.fields.iter().position(|field| *field == selected)
        {
            self.selected_field = index;
        }
    }

    fn shift_fields_for_insert(&mut self, offset: usize, length: usize) {
        for field in &mut self.fields {
            if field.start >= offset {
                field.start = field.start.saturating_add(length);
                field.end = field.end.saturating_add(length);
            } else if field.end >= offset {
                field.end = field.end.saturating_add(length);
            }
        }
    }

    fn shift_fields_for_delete(&mut self, offset: usize, length: usize) {
        if length == 0 {
            return;
        }
        let end = offset.saturating_add(length - 1);
        self.fields.retain_mut(|field| {
            if field.end < offset {
                return true;
            }
            if field.start > end {
                field.start = field.start.saturating_sub(length);
                field.end = field.end.saturating_sub(length);
                return true;
            }
            if field.start < offset {
                if field.end > end {
                    field.end = field.end.saturating_sub(length);
                } else {
                    field.end = offset.saturating_sub(1);
                }
                return field.start <= field.end;
            }
            if field.end > end {
                field.start = offset;
                field.end = field.end.saturating_sub(length);
                return true;
            }
            false
        });
        self.selected_field = self.selected_field.min(self.fields.len().saturating_sub(1));
    }

    fn canonical_path(&self) -> String {
        fs::canonicalize(&self.path)
            .unwrap_or_else(|_| self.path.clone())
            .display()
            .to_string()
    }

    fn binary_identity(&self) -> BinaryIdentity {
        BinaryIdentity {
            file_name: self.file_name(),
            file_path: self.canonical_path(),
            content_id: self.saved_content_id.clone(),
        }
    }

    fn open_overlay(&mut self, path: PathBuf, origin: Option<OverlayMetadata>) {
        self.overlay_foreign = origin
            .as_ref()
            .is_some_and(|metadata| metadata.content_id != self.saved_content_id);
        self.overlay_origin = origin;
        self.overlay_path = Some(path);
    }

    fn restore_automatic_overlay(&mut self) -> Result<Option<PathBuf>, String> {
        if let Some((path, overlay)) =
            find_overlay_for_content(&self.overlay_dir, &self.binary_identity())
        {
            self.apply_overlay_fields(overlay.fields);
            let expected_prefix = format!("{}-overlay-", overlay_name_stem(&self.file_name()));
            let named_after_this_file = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&expected_prefix));
            let same_file = named_after_this_file
                && overlay.metadata.as_ref().is_some_and(|metadata| {
                    metadata.file_name == self.file_name()
                        && metadata.file_path == self.canonical_path()
                });
            if same_file {
                self.open_overlay(path.clone(), overlay.metadata);
                return Ok(Some(path));
            }
            self.overlay_path = None;
            self.overlay_origin = None;
            self.overlay_foreign = false;
            if let Some(forked) = self.persist_automatic_overlay()? {
                return Ok(Some(forked));
            }
            return Ok(Some(path));
        }

        let legacy = self
            .overlay_dir
            .join(format!("{}.json", self.saved_content_id));
        if !legacy.is_file() {
            return Ok(None);
        }
        let overlay = read_overlay(&legacy)?;
        self.apply_overlay_fields(overlay.fields);
        match self.persist_automatic_overlay()? {
            Some(migrated) => {
                let _ = fs::remove_file(&legacy);
                Ok(Some(migrated))
            }
            None => Ok(Some(legacy)),
        }
    }

    fn overlay_metadata(&self) -> OverlayMetadata {
        let now = utc_timestamp(SystemTime::now());
        if self.overlay_foreign
            && let Some(origin) = &self.overlay_origin
        {
            return OverlayMetadata {
                format_version: OVERLAY_FORMAT_VERSION,
                updated: now,
                ..origin.clone()
            };
        }
        OverlayMetadata {
            format_version: OVERLAY_FORMAT_VERSION,
            file_name: self.file_name(),
            file_path: self.canonical_path(),
            file_size: self.buffer.original_storage().len() as u64,
            content_id: self.saved_content_id.clone(),
            created: self
                .overlay_origin
                .as_ref()
                .map(|origin| origin.created.clone())
                .filter(|created| !created.is_empty())
                .unwrap_or_else(|| now.clone()),
            updated: now,
        }
    }

    fn write_overlay(&mut self, path: &Path) -> Result<String, String> {
        if path.starts_with(&self.overlay_dir) {
            fs::create_dir_all(&self.overlay_dir).map_err(|error| {
                format!(
                    "Could not create overlay storage directory {}: {error}",
                    self.overlay_dir.display()
                )
            })?;
        }
        if !self.backed_up_overlays.contains(path) {
            if path.is_file() {
                fs::copy(path, overlay_backup_path(path)).map_err(|error| {
                    format!(
                        "Could not back up {} before saving: {error}",
                        path.display()
                    )
                })?;
            }
            self.backed_up_overlays.insert(path.to_owned());
        }
        let metadata = self.overlay_metadata();
        let json = serde_json::to_string_pretty(&Overlay {
            metadata: Some(metadata.clone()),
            fields: self.fields.clone(),
        })
        .map_err(|error| format!("Could not serialize overlay: {error}"))?;
        fs::write(path, json)
            .map_err(|error| format!("Could not save overlay to {}: {error}", path.display()))?;
        if self.overlay_path.as_deref() == Some(path) {
            self.overlay_origin = Some(metadata);
        }
        Ok(format!("Saved overlay to {}", path.display()))
    }

    fn save_automatic_overlay_after_change(&mut self) {
        match self.persist_automatic_overlay() {
            Ok(Some(path)) => {
                self.status = format!("{}; overlay saved to {}", self.status, path.display());
            }
            Ok(None) => {}
            Err(error) => self.status = format!("{}; {error}", self.status),
        }
    }

    fn apply_overlay_fields(&mut self, mut fields: Vec<Field>) {
        let len = self.buffer.len();
        fields.retain(|field| field.start <= field.end && field.end < len);
        self.fields = fields;
        self.sort_fields();
        self.selected_field = 0;
    }
}

pub fn parse_offset(input: &str) -> Result<usize, String> {
    let input = input.trim().replace('_', "");
    if input.is_empty() {
        return Err("offset is empty".into());
    }
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        usize::from_str_radix(hex, 16).map_err(|_| "invalid hexadecimal offset".into())
    } else {
        input
            .parse::<usize>()
            .map_err(|_| "use decimal or a 0x-prefixed hexadecimal offset".into())
    }
}

fn overlay_storage_dir() -> PathBuf {
    rexedit_data_dir("overlays")
}

fn read_overlay(path: &Path) -> Result<Overlay, String> {
    let json = fs::read_to_string(path)
        .map_err(|error| format!("Could not read overlay {}: {error}", path.display()))?;
    serde_json::from_str(&json).map_err(|error| format!("Invalid overlay JSON: {error}"))
}

fn overlay_name_stem(file_name: &str) -> String {
    let stem: String = file_name
        .chars()
        .enumerate()
        .map(|(index, character)| match character {
            '.' if index == 0 => '_',
            character if character.is_alphanumeric() || matches!(character, '.' | '-' | '_') => {
                character
            }
            _ => '_',
        })
        .collect();
    if stem.is_empty() {
        "binary".into()
    } else {
        stem
    }
}

fn overlay_file_name(file_name: &str, number: usize) -> String {
    format!("{}-overlay-{number}.json", overlay_name_stem(file_name))
}

fn next_free_overlay_path(dir: &Path, file_name: &str) -> PathBuf {
    (1..)
        .map(|number| dir.join(overlay_file_name(file_name, number)))
        .find(|path| !path.exists())
        .expect("an unused overlay number exists")
}

fn allocate_overlay_path(dir: &Path, file_name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let mut number = 1;
    loop {
        let path = dir.join(overlay_file_name(file_name, number));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => number += 1,
            Err(error) => return Err(error),
        }
    }
}

struct BinaryIdentity {
    file_name: String,
    file_path: String,
    content_id: String,
}

fn find_overlay_for_content(dir: &Path, binary: &BinaryIdentity) -> Option<(PathBuf, Overlay)> {
    let likely_prefix = format!("{}-overlay-", overlay_name_stem(&binary.file_name));
    let mut paths = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let rank = |path: &Path, metadata: &OverlayMetadata| {
        let named_after_binary = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(&likely_prefix));
        (
            metadata.file_path == binary.file_path,
            metadata.file_name == binary.file_name,
            named_after_binary,
            metadata.updated.clone(),
        )
    };
    let mut best: Option<(PathBuf, Overlay)> = None;
    for path in paths {
        let Ok(overlay) = read_overlay(&path) else {
            continue;
        };
        let Some(metadata) = &overlay.metadata else {
            continue;
        };
        if metadata.content_id != binary.content_id {
            continue;
        }
        let better = best.as_ref().is_none_or(|(best_path, best_overlay)| {
            best_overlay
                .metadata
                .as_ref()
                .is_none_or(|best_metadata| rank(&path, metadata) > rank(best_path, best_metadata))
        });
        if better {
            best = Some((path, overlay));
        }
    }
    best
}

fn overlay_backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

fn utc_timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, time_of_day) = (seconds / 86_400, seconds % 86_400);
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        time_of_day % 3_600 / 60,
        time_of_day % 60
    )
}

fn field_order(a: &Field, b: &Field) -> std::cmp::Ordering {
    a.start
        .cmp(&b.start)
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FieldColor;

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rex-core-doc-test-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn uncompressed_rows_are_computed_without_allocating_collapsed_rows() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0u8; 64]);
        assert!(!doc.settings.compress_repeated_rows);
        assert!(doc.collapsed_runs.is_empty());
        assert_eq!(doc.row_count(), 4);
        assert_eq!(doc.display_row(2), DisplayRow::Bytes { offset: 32 });
        assert_eq!(doc.display_row_for_offset(35), 2);

        doc.settings.compress_repeated_rows = true;
        doc.rebuild_display_rows();
        assert_eq!(doc.collapsed_runs.len(), 1);
        assert_eq!(doc.row_count(), 1);
        assert_eq!(
            doc.display_row(0),
            DisplayRow::Repeated {
                start: 0,
                end: 63,
                byte: 0,
                physical_rows: 4,
            }
        );

        let mut mixed = vec![0x11u8; 16];
        mixed.extend_from_slice(&[0x00u8; 48]);
        mixed.extend_from_slice(&[0x22u8; 16]);
        let mut doc2 = Document::from_bytes("mixed.bin".into(), mixed);
        doc2.settings.compress_repeated_rows = true;
        doc2.rebuild_display_rows();
        assert_eq!(doc2.collapsed_runs.len(), 1);
        assert_eq!(doc2.row_count(), 3);
        assert_eq!(doc2.display_row(0), DisplayRow::Bytes { offset: 0 });
        assert_eq!(
            doc2.display_row(1),
            DisplayRow::Repeated {
                start: 16,
                end: 63,
                byte: 0,
                physical_rows: 3,
            }
        );
        assert_eq!(doc2.display_row(2), DisplayRow::Bytes { offset: 64 });
        assert_eq!(doc2.display_row_for_offset(0), 0);
        assert_eq!(doc2.display_row_for_offset(20), 1);
        assert_eq!(doc2.display_row_for_offset(63), 1);
        assert_eq!(doc2.display_row_for_offset(64), 2);
        assert_eq!(doc2.display_row_for_offset(79), 2);
    }

    #[test]
    fn nibble_edits_and_undo_redo_round_trip() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0x12, 0x34]);
        doc.edit_mode = true;
        doc.edit_nibble(0xA);
        doc.edit_nibble(0xB);
        assert_eq!(doc.buffer.to_vec(), vec![0xAB, 0x34]);
        assert!(doc.is_dirty());
        assert!(doc.can_undo());

        doc.undo();
        assert_eq!(doc.buffer.to_vec(), vec![0x12, 0x34]);
        assert!(!doc.is_dirty());
        assert!(doc.can_redo());

        doc.redo();
        assert_eq!(doc.buffer.to_vec(), vec![0xAB, 0x34]);
    }

    #[test]
    fn insert_and_delete_shift_overlay_fields() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![1, 2, 3, 4]);
        doc.add_field(Field {
            name: "tail".into(),
            description: String::new(),
            start: 2,
            end: 3,
            color: FieldColor::Cyan,
            background: None,
        });

        doc.insert_bytes(1, &[0xFF, 0xEE]);
        assert_eq!(doc.fields[0].start, 4);
        assert_eq!(doc.fields[0].end, 5);

        doc.selection = Some(Selection::from_range(1, 2));
        doc.delete_selected_bytes();
        assert_eq!(doc.fields[0].start, 2);
        assert_eq!(doc.fields[0].end, 3);
    }

    #[test]
    fn overlay_and_binary_save_round_trip() {
        let dir = temp_dir("roundtrip");
        let binary_path = dir.join("sample.bin");
        let overlay_path = dir.join("custom-overlay.json");
        fs::write(&binary_path, [0xCA, 0xFE, 0xBA, 0xBE]).unwrap();

        let mut doc = Document::with_overlay_dir(
            binary_path.clone(),
            vec![0xCA, 0xFE, 0xBA, 0xBE],
            dir.join("overlays"),
        );
        doc.add_field(Field {
            name: "magic".into(),
            description: "header".into(),
            start: 0,
            end: 3,
            color: FieldColor::Green,
            background: Some(FieldColor::Blue),
        });
        doc.save_overlay_to(&overlay_path).unwrap();

        doc.fields.clear();
        doc.load_overlay_from(&overlay_path).unwrap();
        assert_eq!(doc.fields.len(), 1);
        assert_eq!(doc.fields[0].name, "magic");

        doc.overwrite_bytes(0, &[0xDE, 0xAD]);
        doc.save().unwrap();
        assert_eq!(fs::read(&binary_path).unwrap(), vec![0xDE, 0xAD, 0xBA, 0xBE]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_decimal_and_hex_offsets() {
        assert_eq!(Document::parse_offset("42").unwrap(), 42);
        assert_eq!(Document::parse_offset("0x2A").unwrap(), 42);
        assert_eq!(Document::parse_offset("0x1_000").unwrap(), 0x1000);
        assert!(Document::parse_offset("").is_err());
    }

    #[test]
    fn copy_formats_and_paste_round_trip() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0x4D, 0x5A, 0x90, 0x00]);
        doc.selection = Some(Selection::from_range(0, 3));
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::HexSpaced).as_deref(),
            Some("4D 5A 90 00")
        );
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::HexCompact).as_deref(),
            Some("4D5A9000")
        );
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::Ascii).as_deref(),
            Some("MZ..")
        );
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::CArray).as_deref(),
            Some("{ 0x4D, 0x5A, 0x90, 0x00 }")
        );
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::RustArray).as_deref(),
            Some("[0x4D, 0x5A, 0x90, 0x00]")
        );
        assert_eq!(
            doc.format_selected_bytes(CopyFormat::PythonBytes)
                .as_deref(),
            Some("b\"\\x4d\\x5a\\x90\\x00\"")
        );

        doc.selection = Some(Selection::new(0));
        doc.paste_hex_bytes("{ 0xDE, 0xAD, 0xBE, 0xEF }");
        assert_eq!(doc.buffer.to_vec(), vec![0xDE, 0xAD, 0xBE, 0xEF]);

        doc.selection = Some(Selection::new(0));
        doc.paste_hex_bytes("b\"\\xca\\xfe\"");
        assert_eq!(doc.buffer.to_vec(), vec![0xCA, 0xFE, 0xBE, 0xEF]);

        doc.selection = Some(Selection::new(0));
        doc.paste_ascii_bytes("OK");
        assert_eq!(doc.buffer.to_vec(), vec![b'O', b'K', 0xBE, 0xEF]);
    }

    #[test]
    fn cycle_edit_mode_rotates_view_overwrite_insert() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0x11, 0x22]);
        assert!(!doc.edit_mode);

        doc.cycle_edit_mode();
        assert!(doc.edit_mode);
        assert_eq!(doc.edit_kind, EditKind::Overwrite);

        doc.cycle_edit_mode();
        assert!(doc.edit_mode);
        assert_eq!(doc.edit_kind, EditKind::Insert);

        doc.cycle_edit_mode();
        assert!(!doc.edit_mode);
        assert_eq!(doc.edit_kind, EditKind::Overwrite);
    }

    #[test]
    fn saving_to_new_filename_forks_overlay_and_keeps_original_linked() {
        let dir = temp_dir("fork-overlay");
        let overlay_dir = dir.join("overlays");
        let file_a = dir.join("alpha.bin");
        let file_b = dir.join("beta.bin");
        fs::write(&file_a, [0x10, 0x20, 0x30, 0x40]).unwrap();

        let mut doc =
            Document::with_overlay_dir(file_a.clone(), vec![0x10, 0x20, 0x30, 0x40], overlay_dir.clone());
        doc.add_field(Field {
            name: "alpha_field".into(),
            description: "original".into(),
            start: 0,
            end: 1,
            color: FieldColor::Cyan,
            background: None,
        });
        assert_eq!(
            doc.open_overlay_name().as_deref(),
            Some("alpha.bin-overlay-1.json")
        );

        // Save binary to a new filename and modify bytes/fields on the new file
        doc.overwrite_bytes(0, &[0x99]);
        doc.save_as(file_b.clone()).unwrap();
        assert_eq!(
            doc.open_overlay_name().as_deref(),
            Some("beta.bin-overlay-1.json")
        );

        doc.add_field(Field {
            name: "beta_only_field".into(),
            description: "added after save as".into(),
            start: 2,
            end: 3,
            color: FieldColor::Yellow,
            background: None,
        });

        // Reopen original file_a: must still be linked to alpha.bin-overlay-1.json with only alpha_field
        let reopened_a = Document::with_overlay_dir(
            file_a.clone(),
            fs::read(&file_a).unwrap(),
            overlay_dir.clone(),
        );
        assert_eq!(
            reopened_a.open_overlay_name().as_deref(),
            Some("alpha.bin-overlay-1.json")
        );
        assert_eq!(reopened_a.fields.len(), 1);
        assert_eq!(reopened_a.fields[0].name, "alpha_field");

        // Reopen new file_b: must be linked to beta.bin-overlay-1.json with both fields
        let reopened_b = Document::with_overlay_dir(
            file_b.clone(),
            fs::read(&file_b).unwrap(),
            overlay_dir.clone(),
        );
        assert_eq!(
            reopened_b.open_overlay_name().as_deref(),
            Some("beta.bin-overlay-1.json")
        );
        assert_eq!(reopened_b.fields.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
