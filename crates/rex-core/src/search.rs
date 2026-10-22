use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::buffer::ByteBuffer;
use crate::model::SearchMatch;

const CHUNK: usize = 4 * 1024 * 1024;
const REGEX_OVERLAP: usize = 64 * 1024;
const BATCH: usize = 4096;

#[derive(Debug)]
pub enum SearchMessage {
    Batch(Vec<SearchMatch>),
    Progress(usize),
    Done,
    Error(String),
}

pub struct SearchWorker {
    pub receiver: Receiver<SearchMessage>,
    cancel: Arc<AtomicBool>,
}

impl SearchWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub enum SearchPattern {
    Exact(Vec<u8>),
    /// Each entry is `(mask, expected)` such that `(byte & mask) == expected`.
    Masked(Vec<(u8, u8)>),
    Regex(regex::bytes::Regex),
}

fn is_sep(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == '_' || c == '-'
}

fn parse_hex(s: &str) -> Result<SearchPattern, String> {
    let mut chars: Vec<char> = Vec::new();
    for token in s.split(is_sep).filter(|t| !t.is_empty()) {
        let t = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);
        chars.extend(t.chars());
    }
    if chars.is_empty() {
        return Err("Empty hex query".to_string());
    }
    if !chars.len().is_multiple_of(2) {
        return Err("Hex query must have an even number of digits".to_string());
    }
    let mut out: Vec<(u8, u8)> = Vec::with_capacity(chars.len() / 2);
    for pair in chars.chunks(2) {
        let (a, b) = (pair[0], pair[1]);
        let wild = |c: char| c == '?' || c == '*';
        match (wild(a), wild(b), a.to_digit(16), b.to_digit(16)) {
            (true, true, _, _) => out.push((0x00, 0x00)),
            (false, true, Some(h), _) => out.push((0xF0, (h as u8) << 4)),
            (true, false, _, Some(l)) => out.push((0x0F, l as u8)),
            (false, false, Some(h), Some(l)) => out.push((0xFF, (h * 16 + l) as u8)),
            _ => return Err(format!("Invalid hex byte '{a}{b}'")),
        }
    }
    Ok(from_masked(out))
}

fn from_masked(v: Vec<(u8, u8)>) -> SearchPattern {
    if v.iter().all(|&(mask, _)| mask == 0xFF) {
        SearchPattern::Exact(v.into_iter().map(|(_, exp)| exp).collect())
    } else {
        SearchPattern::Masked(v)
    }
}

fn non_empty(bytes: Vec<u8>) -> Result<SearchPattern, String> {
    if bytes.is_empty() {
        Err("Empty search query".to_string())
    } else {
        Ok(SearchPattern::Exact(bytes))
    }
}

fn parse_uint(s: &str, bits: u32) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("Empty u{bits} query"));
    }
    let cleaned: String = s.chars().filter(|&c| c != '_').collect();
    let val = if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        cleaned.parse::<u64>()
    }
    .map_err(|_| format!("Invalid u{bits} integer '{s}'"))?;
    let max = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    if val > max {
        return Err(format!("Value out of range for u{bits}"));
    }
    Ok(val)
}

pub fn parse_query(query: &str) -> Result<SearchPattern, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Empty search query".to_string());
    }
    if let Some((prefix, rest)) = query.split_once(':') {
        let p = prefix.trim().to_ascii_lowercase();
        match p.as_str() {
            "re" => {
                if rest.is_empty() {
                    return Err("Empty regex".to_string());
                }
                return regex::bytes::Regex::new(rest)
                    .map(SearchPattern::Regex)
                    .map_err(|e| format!("Invalid regex: {e}"));
            }
            "dec" => {
                let n: u128 = rest
                    .trim()
                    .parse()
                    .map_err(|_| "Invalid decimal number (u128 expected)".to_string())?;
                let bytes = n.to_be_bytes();
                let first = bytes
                    .iter()
                    .position(|&b| b != 0)
                    .unwrap_or(bytes.len() - 1);
                return non_empty(bytes[first..].to_vec());
            }
            "bin" => {
                let bits: Vec<char> = rest
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '_')
                    .collect();
                if bits.is_empty() || !bits.len().is_multiple_of(8) {
                    return Err("Binary query must be a non-zero multiple of 8 bits".to_string());
                }
                let mut out = Vec::with_capacity(bits.len() / 8);
                for byte in bits.chunks(8) {
                    let mut v = 0u8;
                    for &c in byte {
                        v <<= 1;
                        match c {
                            '0' => {}
                            '1' => v |= 1,
                            _ => return Err(format!("Invalid binary digit '{c}'")),
                        }
                    }
                    out.push(v);
                }
                return non_empty(out);
            }
            "text" | "ascii" | "utf8" => {
                return non_empty(rest.as_bytes().to_vec());
            }
            "itext" | "iascii" => {
                if rest.is_empty() {
                    return Err("Empty search query".to_string());
                }
                let pat = format!("(?i){}", regex::escape(rest));
                return regex::bytes::Regex::new(&pat)
                    .map(SearchPattern::Regex)
                    .map_err(|e| format!("Invalid regex: {e}"));
            }
            "utf16" | "utf16le" => {
                let bytes: Vec<u8> = rest.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
                return non_empty(bytes);
            }
            "u16le" => {
                let v = parse_uint(rest, 16)? as u16;
                return Ok(SearchPattern::Exact(v.to_le_bytes().to_vec()));
            }
            "u16be" => {
                let v = parse_uint(rest, 16)? as u16;
                return Ok(SearchPattern::Exact(v.to_be_bytes().to_vec()));
            }
            "u32le" => {
                let v = parse_uint(rest, 32)? as u32;
                return Ok(SearchPattern::Exact(v.to_le_bytes().to_vec()));
            }
            "u32be" => {
                let v = parse_uint(rest, 32)? as u32;
                return Ok(SearchPattern::Exact(v.to_be_bytes().to_vec()));
            }
            "u64le" => {
                let v = parse_uint(rest, 64)?;
                return Ok(SearchPattern::Exact(v.to_le_bytes().to_vec()));
            }
            "u64be" => {
                let v = parse_uint(rest, 64)?;
                return Ok(SearchPattern::Exact(v.to_be_bytes().to_vec()));
            }
            "hex" => return parse_hex(rest),
            _ => {}
        }
    }
    parse_hex(query)
}

/// Scan `data` for the pattern. Only matches whose start is `< max_start` are kept.
/// Match offsets are reported as `base + index`.
fn scan(
    data: &[u8],
    pattern: &SearchPattern,
    max_start: usize,
    base: usize,
    out: &mut Vec<SearchMatch>,
) {
    match pattern {
        SearchPattern::Exact(needle) => {
            let plen = needle.len();
            if plen == 0 || data.len() < plen {
                return;
            }
            let finder = memchr::memmem::Finder::new(needle);
            let mut pos = 0usize;
            while pos < data.len() && pos < max_start {
                match finder.find(&data[pos..]) {
                    Some(i) => {
                        let start = pos + i;
                        if start >= max_start {
                            break;
                        }
                        out.push(SearchMatch {
                            start: base + start,
                            end: base + start + plen - 1,
                        });
                        pos = start + 1;
                    }
                    None => break,
                }
            }
        }
        SearchPattern::Masked(mask) => {
            let plen = mask.len();
            if plen == 0 || data.len() < plen {
                return;
            }
            let check = |start: usize| -> bool {
                mask.iter()
                    .zip(&data[start..start + plen])
                    .all(|(&(m, exp), &b)| (b & m) == exp)
            };
            let anchor = mask
                .iter()
                .enumerate()
                .find_map(|(i, &(m, exp))| (m == 0xFF).then_some((i, exp)));
            let mut push = |start: usize| {
                out.push(SearchMatch {
                    start: base + start,
                    end: base + start + plen - 1,
                });
            };
            match anchor {
                Some((ai, ab)) => {
                    for idx in memchr::memchr_iter(ab, data) {
                        if idx < ai {
                            continue;
                        }
                        let start = idx - ai;
                        if start >= max_start {
                            break;
                        }
                        if start + plen <= data.len() && check(start) {
                            push(start);
                        }
                    }
                }
                None => {
                    let last = data.len() - plen;
                    for start in 0..=last {
                        if start >= max_start {
                            break;
                        }
                        if check(start) {
                            push(start);
                        }
                    }
                }
            }
        }
        SearchPattern::Regex(re) => {
            for m in re.find_iter(data) {
                if m.start() >= max_start {
                    break;
                }
                if m.end() == m.start() {
                    continue;
                }
                out.push(SearchMatch {
                    start: base + m.start(),
                    end: base + m.end() - 1,
                });
            }
        }
    }
}

/// Core search loop. `emit` returns false if the receiver is gone.
fn run_search(
    buffer: &ByteBuffer,
    pattern: &SearchPattern,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(SearchMessage) -> bool,
) {
    let total = buffer.len();
    let contiguous = buffer.as_contiguous_slice();
    let mut out: Vec<SearchMatch> = Vec::new();

    let overlap = match pattern {
        SearchPattern::Exact(v) => v.len().saturating_sub(1),
        SearchPattern::Masked(v) => v.len().saturating_sub(1),
        SearchPattern::Regex(_) => REGEX_OVERLAP,
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    while pos < total {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let is_final = pos + CHUNK >= total;
        let want = (CHUNK + overlap).min(total - pos);
        let data: &[u8] = match contiguous {
            Some(s) => &s[pos..pos + want],
            None => {
                buf.resize(want, 0);
                let n = buffer.copy_range(pos, &mut buf);
                &buf[..n]
            }
        };
        let max_start = if is_final { usize::MAX } else { CHUNK };
        scan(data, pattern, max_start, pos, &mut out);
        pos += CHUNK;
        while out.len() >= BATCH {
            let rest = out.split_off(BATCH);
            let batch = std::mem::replace(&mut out, rest);
            if !emit(SearchMessage::Batch(batch)) {
                return;
            }
        }
        if !emit(SearchMessage::Progress(pos.min(total))) {
            return;
        }
    }
    if !out.is_empty() && !emit(SearchMessage::Batch(out)) {
        return;
    }
    emit(SearchMessage::Done);
}

pub fn spawn(buffer: ByteBuffer, query: String) -> Result<SearchWorker, String> {
    let pattern = parse_query(&query)?;
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_t = Arc::clone(&cancel);
    thread::spawn(move || {
        let mut emit = |m: SearchMessage| tx.send(m).is_ok();
        run_search(&buffer, &pattern, &cancel_t, &mut emit);
    });
    Ok(SearchWorker {
        receiver: rx,
        cancel,
    })
}

pub fn search(buffer: &ByteBuffer, query: &str) -> Result<Vec<SearchMatch>, String> {
    let pattern = parse_query(query)?;
    let cancel = AtomicBool::new(false);
    let mut results = Vec::new();
    let mut error = None;
    let mut emit = |m: SearchMessage| {
        match m {
            SearchMessage::Batch(b) => results.extend(b),
            SearchMessage::Error(e) => error = Some(e),
            _ => {}
        }
        true
    };
    run_search(buffer, &pattern, &cancel, &mut emit);
    match error {
        Some(e) => Err(e),
        None => Ok(results),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(start: usize, end: usize) -> SearchMatch {
        SearchMatch { start, end }
    }

    #[test]
    fn hex_exact() {
        let b = ByteBuffer::from_bytes(vec![0, 0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD]);
        assert_eq!(search(&b, "DE AD").unwrap(), vec![m(1, 2), m(5, 6)]);
        assert_eq!(search(&b, "hex:0xDE,0xAD").unwrap(), vec![m(1, 2), m(5, 6)]);
        assert_eq!(search(&b, "deadbeef").unwrap(), vec![m(1, 4)]);
    }

    #[test]
    fn hex_wildcards() {
        let b = ByteBuffer::from_bytes(vec![1, 2, 3, 1, 9, 3]);
        assert_eq!(search(&b, "01 ?? 03").unwrap(), vec![m(0, 2), m(3, 5)]);
        assert_eq!(search(&b, "** 09").unwrap(), vec![m(3, 4)]);
    }

    #[test]
    fn hex_nibble_wildcards() {
        let b = ByteBuffer::from_bytes(vec![0x41, 0x2A, 0x4F, 0xBA, 0x51, 0x2A]);
        assert_eq!(search(&b, "4? ?A").unwrap(), vec![m(0, 1), m(2, 3)]);
        assert_eq!(search(&b, "4* *A 4?").unwrap(), vec![m(0, 2)]);
    }

    #[test]
    fn itext_and_typed_integers() {
        let b = ByteBuffer::from_bytes(b"xxHeLLoyy\x78\x56\x34\x12\x12\x34\x56\x78".to_vec());
        assert_eq!(search(&b, "itext:hello").unwrap(), vec![m(2, 6)]);
        assert_eq!(search(&b, "u32le:0x12345678").unwrap(), vec![m(9, 12)]);
        assert_eq!(search(&b, "u32be:0x12345678").unwrap(), vec![m(13, 16)]);
        assert_eq!(search(&b, "u32le:305419896").unwrap(), vec![m(9, 12)]);
    }

    #[test]
    fn dec_bin_text_and_utf16() {
        let b = ByteBuffer::from_bytes(b"\x01\x00hello\0h\0i\0".to_vec());
        assert_eq!(search(&b, "dec:256").unwrap(), vec![m(0, 1)]);
        assert_eq!(search(&b, "text:hello").unwrap(), vec![m(2, 6)]);
        assert_eq!(search(&b, "utf16:hi").unwrap(), vec![m(8, 11)]);
    }
}
