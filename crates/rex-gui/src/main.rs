// Release GUI builds are Windows-subsystem apps (no stray console on double-click).
// TUI-only builds stay console apps so they run inside the terminal that launched them.
#![cfg_attr(
    all(not(debug_assertions), feature = "gui"),
    windows_subsystem = "windows"
)]

#[cfg(not(any(feature = "gui", feature = "tui")))]
compile_error!("rexeditor needs at least one front end: enable the `gui` and/or `tui` feature");

mod cli;
mod paths;

#[cfg(feature = "gui")]
mod app;
#[cfg(feature = "gui")]
mod clipboard;
#[cfg(feature = "gui")]
mod file_picker;
#[cfg(feature = "gui")]
mod hex_viewport;
#[cfg(feature = "gui")]
mod sidebar;
#[cfg(feature = "gui")]
mod theme;

#[cfg(feature = "tui")]
mod tui;

#[cfg(all(test, feature = "gui"))]
mod test_support;

#[cfg(windows)]
mod win_console;

use std::{env, process::ExitCode};

use cli::{FrontEnd, LaunchOptions};

fn main() -> ExitCode {
    let options = cli::parse_cli_args(env::args_os().skip(1));

    if options.show_help {
        #[cfg(windows)]
        win_console::attach_parent();
        print!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }

    let use_tui = match options.front_end {
        FrontEnd::Tui => true,
        FrontEnd::Auto => !cfg!(feature = "gui") || !cli::graphical_session_available(),
    };

    if use_tui {
        run_tui(options)
    } else {
        run_gui(options)
    }
}

#[cfg(feature = "tui")]
fn run_tui(options: LaunchOptions) -> ExitCode {
    // A release GUI build on Windows has no console of its own; give the TUI one.
    #[cfg(all(windows, feature = "gui", not(debug_assertions)))]
    win_console::alloc_new();

    match tui::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rexeditor: terminal UI error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "tui"))]
fn run_tui(_options: LaunchOptions) -> ExitCode {
    #[cfg(windows)]
    win_console::attach_parent();
    eprintln!(
        "rexeditor: no graphical display found and this build has no terminal UI.\n\
         Rebuild with the `tui` feature (enabled by default) to use `--tui`."
    );
    ExitCode::FAILURE
}

#[cfg(feature = "gui")]
fn run_gui(options: LaunchOptions) -> ExitCode {
    #[cfg(feature = "tui")]
    let fallback = options.clone();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RexEditor — Binary Analysis Workstation")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    // Set once the window is up. Only a window that never opened falls back to the
    // TUI; an error after that (e.g. the display connection dropping) is reported, not
    // papered over with a different interface.
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started_flag = started.clone();
    let result = eframe::run_native(
        "RexEditor",
        native_options,
        Box::new(move |_cc| {
            started_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(app::RexEditorApp::new_with_options(options)))
        }),
    );
    let Err(err) = result else {
        return ExitCode::SUCCESS;
    };
    #[cfg(windows)]
    win_console::attach_parent();
    if started.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("rexeditor: the graphical window closed unexpectedly: {err}");
        return ExitCode::FAILURE;
    }
    eprintln!("rexeditor: could not start the graphical window: {err}");
    #[cfg(feature = "tui")]
    {
        eprintln!("rexeditor: falling back to the terminal UI");
        run_tui(fallback)
    }
    #[cfg(not(feature = "tui"))]
    ExitCode::FAILURE
}

#[cfg(not(feature = "gui"))]
fn run_gui(options: LaunchOptions) -> ExitCode {
    run_tui(options)
}
