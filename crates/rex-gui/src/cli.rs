//! Command-line parsing and launch helpers shared by the GUI and TUI front ends.

use std::{ffi::OsString, path::PathBuf};

use rex_core::Document;

/// Which front end the user asked for on the command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrontEnd {
    /// GUI when available, otherwise the terminal UI.
    #[default]
    Auto,
    /// `--tui`: always use the terminal UI.
    Tui,
}

#[derive(Default, Debug, Clone)]
pub struct LaunchOptions {
    pub paths: Vec<PathBuf>,
    pub initial_offset: Option<usize>,
    pub cli_error: Option<String>,
    pub front_end: FrontEnd,
    pub show_help: bool,
}

pub const USAGE: &str = "\
RexEditor - hex editor and binary analysis workstation

Usage: rexeditor [OPTIONS] [FILEPATH]...

Arguments:
  [FILEPATH]...          Binary file(s) to open

Options:
  -o, --offset <OFFSET>  Jump to decimal or 0x-prefixed hex offset on open
      --tui              Run the terminal UI instead of the graphical window
  -h, --help             Print help
";

pub fn parse_cli_args<I>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = OsString>,
{
    let mut opts = LaunchOptions::default();
    let set_offset = |opts: &mut LaunchOptions, val: &str| match Document::parse_offset(val) {
        Ok(off) => opts.initial_offset = Some(off),
        Err(err) => opts.cli_error = Some(format!("Invalid --offset value '{val}': {err}")),
    };

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let s = arg.to_string_lossy();
        if s == "-o" || s == "--offset" {
            match iter.next() {
                Some(val) => set_offset(&mut opts, &val.to_string_lossy()),
                None => {
                    opts.cli_error = Some("Missing offset value after -o / --offset".to_string());
                }
            }
        } else if let Some(val) = s
            .strip_prefix("--offset=")
            .or_else(|| s.strip_prefix("-o="))
        {
            set_offset(&mut opts, val);
        } else if s == "--tui" {
            opts.front_end = FrontEnd::Tui;
        } else if s == "-h" || s == "--help" {
            opts.show_help = true;
        } else {
            opts.paths.push(PathBuf::from(arg));
        }
    }
    opts
}

pub fn apply_offset_with_bounds_notice(doc: &mut Document, offset: usize) -> Option<String> {
    let len = doc.buffer.len();
    if len == 0 {
        let msg = format!(
            "Offset 0x{offset:X} ({offset}) does not exist because '{}' is empty (0 bytes).",
            doc.file_name()
        );
        doc.status = msg.clone();
        return Some(msg);
    }
    if offset >= len {
        let last = len - 1;
        doc.select_offset(last, false);
        let msg = format!(
            "Offset 0x{offset:X} ({offset}) does not exist in '{}' (file size is {len} bytes / 0x{len:X}); jumped to end of file (0x{last:X}).",
            doc.file_name()
        );
        doc.status = msg.clone();
        Some(msg)
    } else {
        doc.select_offset(offset, false);
        doc.status = format!("Jumped to offset 0x{offset:X} ({offset})");
        None
    }
}

/// True when a graphical session looks available. On Linux and other Unix systems
/// (except macOS) this requires `DISPLAY` or `WAYLAND_DISPLAY`; elsewhere a desktop
/// is assumed.
pub fn graphical_session_available() -> bool {
    if cfg!(all(unix, not(target_os = "macos"))) {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(slice: &[&str]) -> Vec<OsString> {
        slice.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_positional_file_and_hex_or_decimal_offset() {
        let opts = parse_cli_args(args(&["sample.bin", "-o", "0x1A4"]));
        assert_eq!(opts.paths, vec![PathBuf::from("sample.bin")]);
        assert_eq!(opts.initial_offset, Some(0x1A4));
        assert!(opts.cli_error.is_none());
        assert_eq!(opts.front_end, FrontEnd::Auto);

        let opts2 = parse_cli_args(args(&["--offset", "256", "firmware.rom"]));
        assert_eq!(opts2.paths, vec![PathBuf::from("firmware.rom")]);
        assert_eq!(opts2.initial_offset, Some(256));
        assert!(opts2.cli_error.is_none());

        let opts3 = parse_cli_args(args(&["--offset=0xFF", "a.bin"]));
        assert_eq!(opts3.paths, vec![PathBuf::from("a.bin")]);
        assert_eq!(opts3.initial_offset, Some(255));
        assert!(opts3.cli_error.is_none());
    }

    #[test]
    fn parses_tui_flag_and_help() {
        let opts = parse_cli_args(args(&["--tui", "a.bin", "-o", "16"]));
        assert_eq!(opts.front_end, FrontEnd::Tui);
        assert_eq!(opts.paths, vec![PathBuf::from("a.bin")]);
        assert_eq!(opts.initial_offset, Some(16));
        assert!(parse_cli_args(args(&["-h"])).show_help);
    }

    #[test]
    fn reports_invalid_or_missing_offset_argument() {
        let opts = parse_cli_args(args(&["sample.bin", "-o", "0xZZZ"]));
        assert!(opts.cli_error.is_some());

        let opts2 = parse_cli_args(args(&["sample.bin", "--offset"]));
        assert!(opts2.cli_error.is_some());
    }

    #[test]
    fn offset_jumps_to_target_or_clamps_to_eof_with_notice() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0xAA; 64]);

        let warn = apply_offset_with_bounds_notice(&mut doc, 0x20);
        assert!(warn.is_none());
        assert_eq!(doc.selection.map(|s| s.cursor), Some(0x20));
        assert!(doc.scroll_requested);

        let warn_eof = apply_offset_with_bounds_notice(&mut doc, 0x100);
        assert!(warn_eof.is_some());
        assert!(warn_eof.unwrap().contains("does not exist"));
        assert_eq!(doc.selection.map(|s| s.cursor), Some(63));
    }
}
