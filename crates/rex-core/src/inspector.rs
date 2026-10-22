//! Data inspector: decodes the bytes at the cursor or selection into common numeric and text types.

use crate::model::Selection;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Endianness {
    #[default]
    Little,
    Big,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InspectorValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Utf8(String),
    Utf16Le(String),
}

macro_rules! endian_bytes {
    ($v:expr, $e:expr) => {
        match $e {
            Endianness::Little => $v.to_le_bytes().to_vec(),
            Endianness::Big => $v.to_be_bytes().to_vec(),
        }
    };
}

/// Parses a signed or unsigned integer. Accepts decimal or `0x` hex, with an optional leading `-`.
fn parse_i128(input: &str) -> Result<i128, String> {
    let s = input.trim().replace('_', "");
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, s.clone()),
    };
    let mag = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16)
    } else {
        body.parse::<i128>()
    }
    .map_err(|e| format!("invalid integer '{input}': {e}"))?;
    Ok(if neg { -mag } else { mag })
}

macro_rules! parse_int {
    ($input:expr, $endian:expr, $t:ty, $ut:ty) => {{
        let v = parse_i128($input)?;
        let val: $t = if let Ok(x) = <$t>::try_from(v) {
            x
        } else if let Ok(u) = <$ut>::try_from(v) {
            // Hex/bit-pattern input for signed types, e.g. 0xFF as i8 = -1.
            u as $t
        } else {
            return Err(format!("value '{}' out of range for {}", $input, stringify!($t)));
        };
        Ok(endian_bytes!(val, $endian))
    }};
}

impl InspectorValue {
    pub fn to_bytes(&self, endian: Endianness) -> Vec<u8> {
        match self {
            Self::U8(v) => vec![*v],
            Self::I8(v) => vec![*v as u8],
            Self::U16(v) => endian_bytes!(v, endian),
            Self::I16(v) => endian_bytes!(v, endian),
            Self::U32(v) => endian_bytes!(v, endian),
            Self::I32(v) => endian_bytes!(v, endian),
            Self::U64(v) => endian_bytes!(v, endian),
            Self::I64(v) => endian_bytes!(v, endian),
            Self::F32(v) => endian_bytes!(v, endian),
            Self::F64(v) => endian_bytes!(v, endian),
            Self::Utf8(s) => s.as_bytes().to_vec(),
            Self::Utf16Le(s) => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
        }
    }

    pub fn parse_as(kind: &str, input: &str, endian: Endianness) -> Result<Vec<u8>, String> {
        match kind {
            "u8" => parse_int!(input, endian, u8, u8),
            "i8" => parse_int!(input, endian, i8, u8),
            "u16" => parse_int!(input, endian, u16, u16),
            "i16" => parse_int!(input, endian, i16, u16),
            "u32" => parse_int!(input, endian, u32, u32),
            "i32" => parse_int!(input, endian, i32, u32),
            "u64" => parse_int!(input, endian, u64, u64),
            "i64" => parse_int!(input, endian, i64, u64),
            "f32" => input
                .trim()
                .parse::<f32>()
                .map(|v| Self::F32(v).to_bytes(endian))
                .map_err(|e| format!("invalid f32 '{input}': {e}")),
            "f64" => input
                .trim()
                .parse::<f64>()
                .map(|v| Self::F64(v).to_bytes(endian))
                .map_err(|e| format!("invalid f64 '{input}': {e}")),
            "utf8" => Ok(Self::Utf8(input.to_string()).to_bytes(endian)),
            "utf16le" => Ok(Self::Utf16Le(input.to_string()).to_bytes(endian)),
            "hex" => parse_hex_bytes_input(input),
            other => Err(format!("unknown inspector type '{other}'")),
        }
    }
}

/// Parses a hex string (accepting spaces, commas, underscores, and `0x`/`0X` prefixes) into bytes.
pub fn parse_hex_bytes_input(input: &str) -> Result<Vec<u8>, String> {
    let cleaned = input.replace("0x", " ").replace("0X", " ");
    let mut digits = Vec::new();
    for ch in cleaned.chars() {
        if ch.is_ascii_hexdigit() {
            digits.push(ch.to_digit(16).expect("checked hex digit") as u8);
        } else if ch.is_whitespace() || matches!(ch, ',' | ';' | '_' | '-' | '[' | ']' | '{' | '}')
        {
            continue;
        } else {
            return Err(format!("unexpected character '{ch}' in hex input"));
        }
    }
    if digits.is_empty() {
        return Err("no hex digits provided".into());
    }
    if digits.len() % 2 != 0 {
        return Err("hex input must contain an even number of hex digits".into());
    }
    Ok(digits.chunks_exact(2).map(|c| (c[0] << 4) | c[1]).collect())
}

#[derive(Clone, Debug, Default)]
pub struct InspectorData {
    pub range_start: usize,
    pub range_end: usize,
    pub selection_len: usize,
    pub hex_preview: String,
    pub decimal_preview: String,
    pub ascii_preview: String,
    pub binary_preview: String,
    pub u8_val: Option<u8>,
    pub i8_val: Option<i8>,
    pub u16_le: Option<u16>,
    pub u16_be: Option<u16>,
    pub i16_le: Option<i16>,
    pub i16_be: Option<i16>,
    pub u32_le: Option<u32>,
    pub u32_be: Option<u32>,
    pub i32_le: Option<i32>,
    pub i32_be: Option<i32>,
    pub u64_le: Option<u64>,
    pub u64_be: Option<u64>,
    pub i64_le: Option<i64>,
    pub i64_be: Option<i64>,
    pub f32_le: Option<f32>,
    pub f32_be: Option<f32>,
    pub f64_le: Option<f64>,
    pub f64_be: Option<f64>,
    pub uleb128: Option<(u64, usize)>,
    pub sleb128: Option<(i64, usize)>,
    pub unix_timestamp: Option<String>,
    pub utf8_preview: String,
    pub utf16le_preview: String,
}

const PREVIEW_BYTES: usize = 16;
const BINARY_PREVIEW_BYTES: usize = 4;
const TEXT_PREVIEW_BYTES: usize = 64;

fn sanitize_single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    out
}

fn take<const N: usize>(d: &[u8]) -> Option<[u8; N]> {
    d.get(..N).and_then(|s| s.try_into().ok())
}

fn read_uleb128(d: &[u8]) -> Option<(u64, usize)> {
    let mut result = 0u64;
    for (i, &b) in d.iter().take(10).enumerate() {
        result |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

fn read_sleb128(d: &[u8]) -> Option<(i64, usize)> {
    let mut result = 0i64;
    let mut shift = 0u32;
    for (i, &b) in d.iter().take(10).enumerate() {
        result |= i64::from(b & 0x7f) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            if shift < 64 && b & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            return Some((result, i + 1));
        }
    }
    None
}

/// Formats seconds since the Unix epoch as a UTC timestamp (civil-from-days algorithm).
fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

impl InspectorData {
    /// Decodes `window_bytes` (a slice starting at the selection's start offset, e.g., up to 64 bytes).
    pub fn decode(window_bytes: &[u8], selection: Option<Selection>) -> Self {
        let (range_start, range_end, selection_len) = match selection {
            Some(sel) => (sel.start(), sel.end(), sel.len()),
            None => (0, window_bytes.len().saturating_sub(1), window_bytes.len()),
        };
        let d = window_bytes;
        let pv = &d[..d.len().min(PREVIEW_BYTES).min(selection_len.max(1))];
        let bv = &pv[..pv.len().min(BINARY_PREVIEW_BYTES)];
        let tv = &d[..d.len().min(TEXT_PREVIEW_BYTES).min(selection_len.max(1))];
        let join = |f: &dyn Fn(&u8) -> String| pv.iter().map(f).collect::<Vec<_>>().join(" ");

        let utf16: Vec<u16> = tv
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();

        let mut binary_preview = bv
            .iter()
            .map(|b| format!("{b:08b}"))
            .collect::<Vec<_>>()
            .join(" ");
        if selection_len > BINARY_PREVIEW_BYTES {
            binary_preview.push_str(" …");
        }

        Self {
            range_start,
            range_end,
            selection_len,
            hex_preview: join(&|b| format!("{b:02X}")),
            decimal_preview: join(&|b| b.to_string()),
            ascii_preview: pv
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect(),
            binary_preview,
            u8_val: d.first().copied(),
            i8_val: d.first().map(|&b| b as i8),
            u16_le: take(d).map(u16::from_le_bytes),
            u16_be: take(d).map(u16::from_be_bytes),
            i16_le: take(d).map(i16::from_le_bytes),
            i16_be: take(d).map(i16::from_be_bytes),
            u32_le: take(d).map(u32::from_le_bytes),
            u32_be: take(d).map(u32::from_be_bytes),
            i32_le: take(d).map(i32::from_le_bytes),
            i32_be: take(d).map(i32::from_be_bytes),
            u64_le: take(d).map(u64::from_le_bytes),
            u64_be: take(d).map(u64::from_be_bytes),
            i64_le: take(d).map(i64::from_le_bytes),
            i64_be: take(d).map(i64::from_be_bytes),
            f32_le: take(d).map(f32::from_le_bytes),
            f32_be: take(d).map(f32::from_be_bytes),
            f64_le: take(d).map(f64::from_le_bytes),
            f64_be: take(d).map(f64::from_be_bytes),
            uleb128: read_uleb128(d),
            sleb128: read_sleb128(d),
            unix_timestamp: take(d).map(|b| format_unix(i64::from(u32::from_le_bytes(b)))),
            utf8_preview: sanitize_single_line(&String::from_utf8_lossy(tv)),
            utf16le_preview: sanitize_single_line(&String::from_utf16_lossy(&utf16)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_integers_both_endians() {
        let data = InspectorData::decode(&[0x01, 0x02, 0x03, 0x04], None);
        assert_eq!(data.u16_le, Some(0x0201));
        assert_eq!(data.u16_be, Some(0x0102));
        assert_eq!(data.u32_le, Some(0x0403_0201));
        assert_eq!(data.u64_le, None);
        assert_eq!(data.hex_preview, "01 02 03 04");
        assert_eq!(data.selection_len, 4);
    }

    #[test]
    fn parses_values_with_hex_and_endianness() {
        assert_eq!(
            InspectorValue::parse_as("u16", "0x1234", Endianness::Big).unwrap(),
            vec![0x12, 0x34]
        );
        assert_eq!(
            InspectorValue::parse_as("i8", "-1", Endianness::Little).unwrap(),
            vec![0xFF]
        );
        assert_eq!(
            InspectorValue::parse_as("i8", "0xFF", Endianness::Little).unwrap(),
            vec![0xFF]
        );
        assert!(InspectorValue::parse_as("u8", "256", Endianness::Little).is_err());
        assert_eq!(
            InspectorValue::parse_as("utf16le", "A", Endianness::Little).unwrap(),
            vec![0x41, 0x00]
        );
    }

    #[test]
    fn decodes_leb128_and_timestamp() {
        let data = InspectorData::decode(&[0xE5, 0x8E, 0x26], None);
        assert_eq!(data.uleb128, Some((624_485, 3)));
        let neg = InspectorData::decode(&[0x7F], None);
        assert_eq!(neg.sleb128, Some((-1, 1)));
        let ts = InspectorData::decode(&[0, 0, 0, 0], None);
        assert_eq!(
            ts.unix_timestamp.as_deref(),
            Some("1970-01-01 00:00:00 UTC")
        );
    }
}
