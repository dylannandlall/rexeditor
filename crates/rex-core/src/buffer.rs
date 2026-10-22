use memmap2::Mmap;
use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Threshold above which files are memory-mapped rather than read into heap memory.
const MMAP_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

/// Read-only base storage for a binary document.
#[derive(Clone, Debug)]
pub enum BaseStorage {
    Memory(Arc<[u8]>),
    Mapped(Arc<Mmap>),
}

impl BaseStorage {
    pub fn open(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        let len = metadata.len();
        if len >= MMAP_THRESHOLD_BYTES {
            let file = File::open(path)?;
            // SAFETY: Read-only mapping of a local file; saving always writes to a temp file
            // and swaps atomically after releasing the mapping.
            if let Ok(mmap) = unsafe { Mmap::map(&file) } {
                return Ok(Self::Mapped(Arc::new(mmap)));
            }
        }
        let bytes = fs::read(path)?;
        Ok(Self::Memory(Arc::from(bytes)))
    }

    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Memory(Arc::from(bytes.into()))
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Memory(buf) => buf,
            Self::Mapped(mmap) => mmap,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceSource {
    Original,
    Added,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub source: PieceSource,
    pub start: usize,
    pub len: usize,
    pub logical_offset: usize,
}

/// Piece-table backed binary buffer supporting instant opening of multi-GB files
/// and fast non-destructive edits without copying the underlying buffer.
#[derive(Clone, Debug)]
pub struct ByteBuffer {
    original: BaseStorage,
    added: Arc<Vec<u8>>,
    pieces: Vec<Piece>,
    total_len: usize,
}

impl Default for ByteBuffer {
    fn default() -> Self {
        Self::from_bytes(Vec::new())
    }
}

impl ByteBuffer {
    pub fn open(path: &Path) -> io::Result<Self> {
        let original = BaseStorage::open(path)?;
        Ok(Self::from_storage(original))
    }

    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::from_storage(BaseStorage::from_bytes(bytes))
    }

    pub fn from_storage(original: BaseStorage) -> Self {
        let total_len = original.len();
        let pieces = if total_len > 0 {
            vec![Piece {
                source: PieceSource::Original,
                start: 0,
                len: total_len,
                logical_offset: 0,
            }]
        } else {
            Vec::new()
        };
        Self {
            original,
            added: Arc::new(Vec::new()),
            pieces,
            total_len,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.total_len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    #[inline]
    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    #[inline]
    pub fn original_storage(&self) -> &BaseStorage {
        &self.original
    }

    /// Returns a direct contiguous slice if the buffer has not been fragmented by edits.
    #[inline]
    pub fn as_contiguous_slice(&self) -> Option<&[u8]> {
        if self.total_len == 0 {
            return Some(&[]);
        }
        if self.pieces.len() == 1 {
            let piece = self.pieces[0];
            return Some(self.slice_for_piece(piece));
        }
        None
    }

    #[inline]
    fn slice_for_piece(&self, piece: Piece) -> &[u8] {
        match piece.source {
            PieceSource::Original => &self.original.as_slice()[piece.start..piece.start + piece.len],
            PieceSource::Added => &self.added[piece.start..piece.start + piece.len],
        }
    }

    /// Finds the index of the piece containing `offset` in $O(\log P)$ time.
    #[inline]
    fn piece_index_for(&self, offset: usize) -> Option<usize> {
        if offset >= self.total_len {
            return None;
        }
        let idx = self
            .pieces
            .partition_point(|piece| piece.logical_offset + piece.len <= offset);
        (idx < self.pieces.len()).then_some(idx)
    }

    /// Reads a single byte at `offset` in $O(\log P)$ time.
    #[inline]
    pub fn get(&self, offset: usize) -> Option<u8> {
        let idx = self.piece_index_for(offset)?;
        let piece = self.pieces[idx];
        let inner = offset - piece.logical_offset;
        Some(self.slice_for_piece(piece)[inner])
    }

    /// Copies up to `out.len()` bytes starting at `start` into `out`, returning the number of bytes copied.
    pub fn copy_range(&self, start: usize, out: &mut [u8]) -> usize {
        if start >= self.total_len || out.is_empty() {
            return 0;
        }
        let available = (self.total_len - start).min(out.len());
        let Some(mut idx) = self.piece_index_for(start) else {
            return 0;
        };
        let mut written = 0;
        let mut current_offset = start;
        while written < available && idx < self.pieces.len() {
            let piece = self.pieces[idx];
            let inner_start = current_offset - piece.logical_offset;
            let piece_rem = piece.len - inner_start;
            let to_copy = piece_rem.min(available - written);
            let src = &self.slice_for_piece(piece)[inner_start..inner_start + to_copy];
            out[written..written + to_copy].copy_from_slice(src);
            written += to_copy;
            current_offset += to_copy;
            idx += 1;
        }
        written
    }

    /// Reads a byte range `[start..start + len)` into a `Vec<u8>`.
    pub fn read_range(&self, start: usize, len: usize) -> Vec<u8> {
        if start >= self.total_len || len == 0 {
            return Vec::new();
        }
        let actual_len = (self.total_len - start).min(len);
        let mut buf = vec![0u8; actual_len];
        self.copy_range(start, &mut buf);
        buf
    }

    /// Materializes the entire buffer as a `Vec<u8>` (useful for small files or tests).
    pub fn to_vec(&self) -> Vec<u8> {
        self.read_range(0, self.total_len)
    }

    /// Returns `true` if the byte at `offset` was modified or inserted relative to the saved base.
    #[inline]
    pub fn is_modified(&self, offset: usize) -> bool {
        let Some(idx) = self.piece_index_for(offset) else {
            return false;
        };
        let piece = self.pieces[idx];
        if piece.source == PieceSource::Original {
            return false;
        }
        let inner = offset - piece.logical_offset;
        let current_byte = self.slice_for_piece(piece)[inner];
        self.original.as_slice().get(offset).copied() != Some(current_byte)
    }

    /// Returns `true` if the buffer contains any unsaved edits.
    pub fn is_dirty(&self) -> bool {
        if self.total_len != self.original.len() {
            return true;
        }
        if self.total_len == 0 {
            return false;
        }
        if self.pieces.len() == 1
            && self.pieces[0].source == PieceSource::Original
            && self.pieces[0].start == 0
        {
            return false;
        }
        self.pieces.iter().any(|piece| {
            if piece.source == PieceSource::Original {
                return piece.start != piece.logical_offset;
            }
            let current = self.slice_for_piece(*piece);
            let orig = &self.original.as_slice()
                [piece.logical_offset..piece.logical_offset + piece.len];
            current != orig
        })
    }

    /// Overwrites bytes starting at `offset` with `data` (clamped to buffer length),
    /// returning the previous bytes for undo.
    pub fn overwrite(&mut self, offset: usize, data: &[u8]) -> Vec<u8> {
        if offset >= self.total_len || data.is_empty() {
            return Vec::new();
        }
        let write_len = data.len().min(self.total_len - offset);
        let data = &data[..write_len];
        let previous = self.read_range(offset, write_len);
        if previous == data {
            return previous;
        }
        self.delete_internal(offset, write_len);
        if self
            .original
            .as_slice()
            .get(offset..offset + write_len)
            .is_some_and(|orig| orig == data)
        {
            self.insert_piece_internal(
                offset,
                Piece {
                    source: PieceSource::Original,
                    start: offset,
                    len: write_len,
                    logical_offset: offset,
                },
            );
        } else {
            self.insert_internal(offset, data);
        }
        previous
    }

    /// Inserts `data` at `offset` (where `offset <= self.len()`).
    pub fn insert(&mut self, offset: usize, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let offset = offset.min(self.total_len);
        self.insert_internal(offset, data);
    }

    /// Deletes up to `len` bytes starting at `offset`, returning the removed bytes for undo.
    pub fn delete(&mut self, offset: usize, len: usize) -> Vec<u8> {
        if offset >= self.total_len || len == 0 {
            return Vec::new();
        }
        let delete_len = len.min(self.total_len - offset);
        let removed = self.read_range(offset, delete_len);
        self.delete_internal(offset, delete_len);
        removed
    }

    fn insert_internal(&mut self, offset: usize, data: &[u8]) {
        let added = Arc::make_mut(&mut self.added);
        let added_start = added.len();
        added.extend_from_slice(data);
        let new_piece = Piece {
            source: PieceSource::Added,
            start: added_start,
            len: data.len(),
            logical_offset: offset,
        };
        self.insert_piece_internal(offset, new_piece);
    }

    fn insert_piece_internal(&mut self, offset: usize, new_piece: Piece) {
        if self.pieces.is_empty() {
            self.pieces.push(new_piece);
            self.total_len = new_piece.len;
            return;
        }

        if offset == self.total_len {
            if let Some(last) = self.pieces.last_mut()
                && last.source == new_piece.source
                && last.start + last.len == new_piece.start
            {
                last.len += new_piece.len;
                self.total_len += new_piece.len;
                return;
            }
            self.pieces.push(new_piece);
            self.total_len += new_piece.len;
            return;
        }

        let idx = self
            .piece_index_for(offset)
            .expect("offset is < total_len");
        let piece = self.pieces[idx];
        let rel = offset - piece.logical_offset;

        if rel == 0 {
            self.pieces.insert(idx, new_piece);
        } else {
            let left = Piece {
                source: piece.source,
                start: piece.start,
                len: rel,
                logical_offset: piece.logical_offset,
            };
            let right = Piece {
                source: piece.source,
                start: piece.start + rel,
                len: piece.len - rel,
                logical_offset: offset + new_piece.len,
            };
            self.pieces.splice(idx..=idx, [left, new_piece, right]);
        }
        self.recompute_offsets_and_coalesce();
    }

    fn delete_internal(&mut self, offset: usize, len: usize) {
        let delete_end = offset + len;
        let mut updated = Vec::with_capacity(self.pieces.len() + 1);

        for piece in &self.pieces {
            let p_start = piece.logical_offset;
            let p_end = p_start + piece.len;

            if p_end <= offset || p_start >= delete_end {
                updated.push(*piece);
            } else {
                if p_start < offset {
                    let left_len = offset - p_start;
                    updated.push(Piece {
                        source: piece.source,
                        start: piece.start,
                        len: left_len,
                        logical_offset: p_start,
                    });
                }
                if p_end > delete_end {
                    let right_skip = delete_end - p_start;
                    updated.push(Piece {
                        source: piece.source,
                        start: piece.start + right_skip,
                        len: piece.len - right_skip,
                        logical_offset: offset,
                    });
                }
            }
        }

        self.pieces = updated;
        self.recompute_offsets_and_coalesce();
    }

    fn recompute_offsets_and_coalesce(&mut self) {
        let mut coalesced: Vec<Piece> = Vec::with_capacity(self.pieces.len());
        let mut offset = 0usize;
        for mut piece in self.pieces.drain(..) {
            if piece.len == 0 {
                continue;
            }
            piece.logical_offset = offset;
            if let Some(prev) = coalesced.last_mut()
                && prev.source == piece.source
                && prev.start + prev.len == piece.start
            {
                prev.len += piece.len;
            } else {
                coalesced.push(piece);
            }
            offset += piece.len;
        }
        self.pieces = coalesced;
        self.total_len = offset;
    }

    /// Streams the current contents of the buffer to `writer`.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        for piece in &self.pieces {
            writer.write_all(self.slice_for_piece(*piece))?;
        }
        Ok(())
    }

    /// Saves the buffer atomically to `path` via a temporary file in the target directory,
    /// releasing any active memory map before replacing the target file and reloading the base.
    pub fn save_to_path(&mut self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "binary".into());
        let temp_path = parent.join(format!(
            ".{file_name}.rextmp-{}-{nonce}",
            std::process::id()
        ));

        {
            let file = File::create(&temp_path)?;
            let mut writer = BufWriter::with_capacity(1024 * 1024, file);
            self.write_to(&mut writer)?;
            writer.flush()?;
            writer.into_inner()?.sync_all()?;
        }

        // Release any active memory-map on `original` before replacing `path` on Windows.
        let backup_original = std::mem::replace(&mut self.original, BaseStorage::from_bytes(Vec::new()));
        drop(backup_original);

        if let Err(rename_err) = fs::rename(&temp_path, path) {
            if let Err(_copy_err) = fs::copy(&temp_path, path) {
                if let Ok(restored) = BaseStorage::open(path) {
                    self.original = restored;
                }
                let _ = fs::remove_file(&temp_path);
                return Err(rename_err);
            }
            let _ = fs::remove_file(&temp_path);
        }

        let reloaded = BaseStorage::open(path)?;
        *self = Self::from_storage(reloaded);
        Ok(())
    }

    /// Computes a fast content identity string (compatible with `rexedit`'s FNV-1a 64-bit hex format).
    /// For files > 16 MiB, hashes the first 8 MiB, middle 4 MiB, last 4 MiB, and file length so
    /// opening a 2 GB file never stalls the UI thread.
    pub fn content_identity(&self) -> String {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;
        const FULL_HASH_LIMIT: usize = 16 * 1024 * 1024;

        let mut hash = FNV_OFFSET;
        let mut feed = |slice: &[u8]| {
            for &byte in slice {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };

        if self.total_len <= FULL_HASH_LIMIT {
            if let Some(slice) = self.as_contiguous_slice() {
                feed(slice);
            } else {
                for piece in &self.pieces {
                    feed(self.slice_for_piece(*piece));
                }
            }
        } else {
            let head = self.read_range(0, 8 * 1024 * 1024);
            let mid = self.read_range(self.total_len / 2, 4 * 1024 * 1024);
            let tail = self.read_range(self.total_len - 4 * 1024 * 1024, 4 * 1024 * 1024);
            feed(&head);
            feed(&mid);
            feed(&tail);
        }

        format!("{:016x}-{:x}", hash, self.total_len)
    }

    #[inline]
    fn diff_at(a: &ByteBuffer, b: &ByteBuffer, offset: usize) -> bool {
        a.get(offset) != b.get(offset)
    }

    fn find_first_diff_in_range(
        &self,
        other: &ByteBuffer,
        start: usize,
        end: usize,
    ) -> Option<usize> {
        if start >= end {
            return None;
        }
        let common_end = end.min(self.total_len).min(other.total_len);
        if start < common_end {
            if let (Some(a), Some(b)) = (self.as_contiguous_slice(), other.as_contiguous_slice()) {
                let a_slice = &a[start..common_end];
                let b_slice = &b[start..common_end];
                if a_slice != b_slice
                    && let Some(rel) = a_slice.iter().zip(b_slice.iter()).position(|(x, y)| x != y)
                {
                    return Some(start + rel);
                }
            } else {
                const CHUNK: usize = 64 * 1024;
                let mut buf_a = vec![0u8; CHUNK.min(common_end - start)];
                let mut buf_b = vec![0u8; CHUNK.min(common_end - start)];
                let mut pos = start;
                while pos < common_end {
                    let len = (common_end - pos).min(CHUNK);
                    self.copy_range(pos, &mut buf_a[..len]);
                    other.copy_range(pos, &mut buf_b[..len]);
                    if buf_a[..len] != buf_b[..len]
                        && let Some(rel) = buf_a[..len]
                            .iter()
                            .zip(buf_b[..len].iter())
                            .position(|(x, y)| x != y)
                    {
                        return Some(pos + rel);
                    }
                    pos += len;
                }
            }
        }
        if common_end < end {
            return Some(start.max(common_end));
        }
        None
    }

    fn find_last_diff_in_range(
        &self,
        other: &ByteBuffer,
        start: usize,
        end: usize,
    ) -> Option<usize> {
        if start >= end {
            return None;
        }
        let common_end = end.min(self.total_len).min(other.total_len);
        if end > common_end {
            return Some(end - 1);
        }
        if let (Some(a), Some(b)) = (self.as_contiguous_slice(), other.as_contiguous_slice()) {
            let a_slice = &a[start..end];
            let b_slice = &b[start..end];
            if a_slice != b_slice
                && let Some(rel) = a_slice
                    .iter()
                    .zip(b_slice.iter())
                    .rposition(|(x, y)| x != y)
            {
                return Some(start + rel);
            }
            return None;
        }
        const CHUNK: usize = 64 * 1024;
        let mut buf_a = vec![0u8; CHUNK.min(end - start)];
        let mut buf_b = vec![0u8; CHUNK.min(end - start)];
        let mut cursor = end;
        while cursor > start {
            let chunk_start = cursor.saturating_sub(CHUNK).max(start);
            let len = cursor - chunk_start;
            self.copy_range(chunk_start, &mut buf_a[..len]);
            other.copy_range(chunk_start, &mut buf_b[..len]);
            if buf_a[..len] != buf_b[..len]
                && let Some(rel) = buf_a[..len]
                    .iter()
                    .zip(buf_b[..len].iter())
                    .rposition(|(x, y)| x != y)
            {
                return Some(chunk_start + rel);
            }
            cursor = chunk_start;
        }
        None
    }

    /// Scans forward from `start_from` (wrapping around to `0` if needed) to find the next
    /// contiguous range of differing bytes `(diff_start, diff_end)` (inclusive, capped at 4096 bytes).
    pub fn find_next_diff(&self, other: &ByteBuffer, start_from: usize) -> Option<(usize, usize)> {
        let max_len = self.len().max(other.len());
        if max_len == 0 {
            return None;
        }
        let pivot = start_from.min(max_len);
        let diff_start = self
            .find_first_diff_in_range(other, pivot, max_len)
            .or_else(|| {
                (pivot > 0)
                    .then(|| self.find_first_diff_in_range(other, 0, pivot))
                    .flatten()
            })?;
        let mut diff_end = diff_start;
        let limit = (diff_start + 4096).min(max_len);
        while diff_end + 1 < limit && Self::diff_at(self, other, diff_end + 1) {
            diff_end += 1;
        }
        Some((diff_start, diff_end))
    }

    /// Scans backward from `before_offset` (wrapping around to the end if needed) to find the
    /// previous contiguous range of differing bytes `(diff_start, diff_end)` (inclusive, capped at 4096 bytes).
    pub fn find_prev_diff(
        &self,
        other: &ByteBuffer,
        before_offset: usize,
    ) -> Option<(usize, usize)> {
        let max_len = self.len().max(other.len());
        if max_len == 0 {
            return None;
        }
        let pivot = before_offset.min(max_len);
        let diff_end = self
            .find_last_diff_in_range(other, 0, pivot)
            .or_else(|| {
                (pivot < max_len)
                    .then(|| self.find_last_diff_in_range(other, pivot, max_len))
                    .flatten()
            })?;
        let mut diff_start = diff_end;
        let min_start = diff_end.saturating_sub(4095);
        while diff_start > min_start && Self::diff_at(self, other, diff_start - 1) {
            diff_start -= 1;
        }
        Some((diff_start, diff_end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piece_table_supports_insert_overwrite_and_delete() {
        let mut buf = ByteBuffer::from_bytes(vec![0x10, 0x20, 0x30, 0x40]);
        assert!(!buf.is_dirty());

        // Overwrite middle byte
        let old = buf.overwrite(1, &[0xFF]);
        assert_eq!(old, vec![0x20]);
        assert_eq!(buf.to_vec(), vec![0x10, 0xFF, 0x30, 0x40]);
        assert!(buf.is_modified(1));
        assert!(!buf.is_modified(0));
        assert!(buf.is_dirty());

        // Insert at start
        buf.insert(0, &[0xAA, 0xBB]);
        assert_eq!(buf.to_vec(), vec![0xAA, 0xBB, 0x10, 0xFF, 0x30, 0x40]);
        assert_eq!(buf.len(), 6);

        // Delete across piece boundaries
        let deleted = buf.delete(1, 3);
        assert_eq!(deleted, vec![0xBB, 0x10, 0xFF]);
        assert_eq!(buf.to_vec(), vec![0xAA, 0x30, 0x40]);
    }

    #[test]
    fn piece_table_coalesces_sequential_appends() {
        let mut buf = ByteBuffer::from_bytes(Vec::new());
        for b in 0..10u8 {
            buf.insert(buf.len(), &[b]);
        }
        assert_eq!(buf.pieces().len(), 1);
        assert_eq!(buf.to_vec(), (0..10u8).collect::<Vec<_>>());
    }

    #[test]
    fn find_next_and_prev_diff_supports_wrap_around_and_length_mismatch() {
        let mut a = ByteBuffer::from_bytes(vec![0, 1, 2, 3, 4, 5, 6, 7]);
        let mut b = ByteBuffer::from_bytes(vec![0, 9, 9, 3, 4, 8, 6, 7, 10, 11]);

        assert_eq!(a.find_next_diff(&b, 0), Some((1, 2)));
        assert_eq!(a.find_next_diff(&b, 3), Some((5, 5)));
        assert_eq!(a.find_next_diff(&b, 6), Some((8, 9)));
        // Wrap around from end to first diff
        assert_eq!(a.find_next_diff(&b, 10), Some((1, 2)));

        // Backward search and wrap around
        assert_eq!(a.find_prev_diff(&b, 10), Some((8, 9)));
        assert_eq!(a.find_prev_diff(&b, 8), Some((5, 5)));
        assert_eq!(a.find_prev_diff(&b, 5), Some((1, 2)));
        assert_eq!(a.find_prev_diff(&b, 1), Some((8, 9)));

        // Fragmented piece-table buffers produce identical diff ranges
        a.overwrite(1, &[1]);
        b.insert(2, &[9]);
        b.delete(2, 1);
        assert_eq!(a.find_next_diff(&b, 0), Some((1, 2)));
        assert_eq!(a.find_prev_diff(&b, 5), Some((1, 2)));

        // Identical buffers return None
        let same = ByteBuffer::from_bytes(vec![1, 2, 3]);
        assert_eq!(same.find_next_diff(&same, 0), None);
        assert_eq!(same.find_prev_diff(&same, 2), None);
    }
}
