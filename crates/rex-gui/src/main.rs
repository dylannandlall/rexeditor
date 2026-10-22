#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod hex_viewport;
mod sidebar;
mod theme;

use std::{env, ffi::OsString, path::PathBuf};

use app::{LaunchOptions, RexEditorApp};
use rex_core::Document;

fn parse_cli_args<I>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = OsString>,
{
    let mut paths = Vec::new();
    let mut initial_offset = None;
    let mut cli_error = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let s = arg.to_string_lossy();
        if s == "-o" || s == "--offset" {
            match iter.next() {
                Some(val) => {
                    let val_str = val.to_string_lossy();
                    match Document::parse_offset(&val_str) {
                        Ok(off) => initial_offset = Some(off),
                        Err(err) => {
                            cli_error =
                                Some(format!("Invalid --offset value '{val_str}': {err}"));
                        }
                    }
                }
                None => {
                    cli_error = Some("Missing offset value after -o / --offset".to_string());
                }
            }
        } else if let Some(val_str) = s
            .strip_prefix("--offset=")
            .or_else(|| s.strip_prefix("-o="))
        {
            match Document::parse_offset(val_str) {
                Ok(off) => initial_offset = Some(off),
                Err(err) => {
                    cli_error = Some(format!("Invalid --offset value '{val_str}': {err}"));
                }
            }
        } else if s == "-h" || s == "--help" {
            println!("Usage: rexeditor [OPTIONS] [FILEPATH]...");
            println!();
            println!("Arguments:");
            println!("  [FILEPATH]...          Binary file(s) to open");
            println!();
            println!("Options:");
            println!("  -o, --offset <OFFSET>  Jump to decimal or 0x-prefixed hex offset on open");
            println!("  -h, --help             Print help");
            std::process::exit(0);
        } else {
            paths.push(PathBuf::from(arg));
        }
    }

    LaunchOptions {
        paths,
        initial_offset,
        cli_error,
    }
}

fn main() -> eframe::Result<()> {
    let options = parse_cli_args(env::args_os().skip(1));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RexEditor — Binary Analysis Workstation")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "RexEditor",
        native_options,
        Box::new(move |_cc| Ok(Box::new(RexEditorApp::new_with_options(options)))),
    )
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
    fn reports_invalid_or_missing_offset_argument() {
        let opts = parse_cli_args(args(&["sample.bin", "-o", "0xZZZ"]));
        assert!(opts.cli_error.is_some());

        let opts2 = parse_cli_args(args(&["sample.bin", "--offset"]));
        assert!(opts2.cli_error.is_some());
    }

    #[test]
    fn offset_jumps_to_target_or_clamps_to_eof_with_notice() {
        let mut doc = Document::from_bytes("sample.bin".into(), vec![0xAA; 64]);

        let warn = app::apply_offset_with_bounds_notice(&mut doc, 0x20);
        assert!(warn.is_none());
        assert_eq!(doc.selection.map(|s| s.cursor), Some(0x20));
        assert!(doc.scroll_requested);

        let warn_eof = app::apply_offset_with_bounds_notice(&mut doc, 0x100);
        assert!(warn_eof.is_some());
        assert!(warn_eof.unwrap().contains("does not exist"));
        assert_eq!(doc.selection.map(|s| s.cursor), Some(63));
    }
}
