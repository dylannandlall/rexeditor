# RexEditor — Hex Viewer & Editor

`rexeditor` is a fast, cross-platform hex viewer and editor for Windows and Linux, with both a
desktop GUI and a terminal UI. It is built for inspecting and patching binaries: it opens
multi-gigabyte files instantly, searches them in the background, decodes selections into
common data types, and lets you annotate regions of a file with colored overlay fields.

Under the hood, a UI-agnostic Rust engine (`crates/rex-core`) drives both front ends
(`crates/rex-gui`), so the GUI and the terminal UI behave the same way and share overlay files.

---

## Features

### Viewing and editing
- **Hex + ASCII view** with configurable bytes per row and eight byte-coloring modes (plain,
  alternating, byte class, high/low nibble, zero bytes, printable, value bands).
- **Overwrite and insert editing** directly in the hex or ASCII column, with full undo/redo.
- **Atomic saves**, so an interrupted write can't leave a half-written file.
- **Multi-tab workspace**, plus **split view** and **diff view** for comparing two files (GUI).
- **Compress repeated rows** to collapse long runs of identical data.
- **Jump to offset** in decimal or `0x` hex, and open a file at an offset from the command line.

### Large files
- Files of 4 MiB or more are memory-mapped read-only instead of being read into RAM, so
  multi-gigabyte files open instantly and stay light on memory.
- Edits live in an in-memory piece table, so inserting or deleting bytes never shifts or copies
  the underlying file, and modified bytes are tracked without rescanning the file.
- Rows are computed on demand, so scrolling cost doesn't grow with file size.
- The GUI is event-driven and sits at 0% CPU when idle.

### Search
- Runs in the background and streams results, so the UI never blocks.
- Query formats: hex (`DE AD BE EF`), hex with wildcards (`DE ?? BE`), text (`text:`),
  case-insensitive text (`itext:`), UTF-16LE (`utf16le:`), decimal (`dec:`), binary (`bin:`),
  fixed-width integers such as `u32le:123`, and regular expressions (`re:[A-Z]+`).
- SIMD-accelerated literal matching for fast scans of large files.

### Data Inspector
- Decodes the selection as little- and big-endian integers, floats, LEB128, UTF-8, UTF-16LE and
  Unix timestamps.
- Click a value to copy it; double-click to edit it in place and write the bytes back.
- **Inspector plugins:** drop an executable script into the plugin directory and it adds a row
  to the Inspector. The selected bytes are sent on stdin, and the first line of stdout becomes
  the value.

### Overlay fields
- Annotate byte ranges with named fields, each with its own border, text and background color
  and a visibility toggle.
- Overlays are saved to JSON files outside the binary and restored automatically when you
  reopen the file, so annotations never modify the file you are analysing.

### Analysis tools (GUI)
- **Entropy view:** Shannon entropy plot and byte-frequency histogram, computed in the background.
- **Python console:** a persistent Python 3 REPL for scripting against the open file.

### Terminal UI
Run `rexeditor --tui [FILEPATH]...` to use the editor inside a terminal, with no X11, Wayland or
OpenGL required, so it works over SSH and on server installs. A build that includes the GUI also
falls back to the terminal UI on Linux when neither `DISPLAY` nor `WAYLAND_DISPLAY` is set.

The terminal UI shares the engine and overlay files with the GUI:

- Hex/ASCII view with overwrite and insert editing and undo/redo.
- Search (`/`, then `n`/`N`), jump to offset (`:`), Data Inspector, and overlay fields.
- Mouse clicks, drag-selection and wheel scrolling in terminals that support them.
- Copy (`y` / `Ctrl+C`) through the terminal clipboard (OSC 52), which also works over SSH.
- Shell-style Tab path completion in the Open (`Ctrl+O`) and Save-as prompts.
- Bytes per row fit the terminal width (16 by default) and can be changed manually.

Press `?` inside the TUI for every keybinding. The entropy view, Python console, inspector
plugins and split/diff view are GUI-only for now.

---

## Building

You need a recent stable Rust toolchain (the workspace uses the 2024 edition).

```bash
cargo test --workspace
cargo build --release --bin rexeditor
```

### Running

```bash
./target/release/rexeditor [--tui] [FILEPATH]... [-o|--offset <0xHEX_OR_DEC>]
```

On Windows the binary is `.\target\release\rexeditor.exe`, with the same options.

### Linux

`rexeditor` loads X11, Wayland and OpenGL at runtime, uses pure-Rust XDG Desktop Portals for file
dialogs, and handles the clipboard in pure Rust. **No `-dev` C header packages (such as
`libgtk-3-dev` or `libx11-dev`) are needed to build it.** At runtime the GUI needs a working X11
or Wayland session with OpenGL; the terminal UI needs only a terminal.

### Terminal-only build

To build without any GUI dependencies (eframe, winit, OpenGL, X11/Wayland), for example for a
server or minimal install:

```bash
cargo build --release -p rex-gui --no-default-features --features tui
./target/release/rexeditor [FILEPATH]
```

This binary starts the terminal UI directly. The Cargo features are `gui` and `tui`, both on by
default; `--no-default-features --features gui` builds the GUI alone.

### Windows notes

Release builds that include the GUI open the terminal UI in a new console window, because Windows
shells don't wait for GUI-subsystem programs and would compete for keyboard input. Build the
terminal-only binary to run the TUI inside an existing terminal.

---

## Project layout

```text
rexeditor/
├── Cargo.toml                   # Workspace manifest
└── crates/
    ├── rex-core/                # Headless binary engine (shared by GUI and TUI)
    │   └── src/
    │       ├── buffer.rs        # Memory-mapped base + in-memory piece table
    │       ├── document.rs      # Row projection, undo/redo, overlays, atomic save
    │       ├── model.rs         # Selection, fields, overlays, theme, byte color modes
    │       ├── inspector.rs     # Integer, float, LEB128, UTF-8/16, timestamp decoding
    │       ├── search.rs        # Background literal, wildcard and regex search
    │       ├── entropy.rs       # Entropy and byte-frequency worker
    │       ├── plugins.rs       # External-script inspector plugins
    │       └── python.rs        # Persistent Python 3 REPL bridge
    │
    └── rex-gui/                 # `rexeditor` binary
        └── src/
            ├── main.rs          # Entry point: picks GUI or TUI
            ├── cli.rs           # Command-line parsing shared by both front ends
            ├── app.rs           # [gui] Tabs, menus, shortcuts, search and Python docks
            ├── hex_viewport.rs  # [gui] Virtualized hex + ASCII painter
            ├── sidebar.rs       # [gui] Data Inspector, field editor, entropy plot
            ├── theme.rs         # [gui] Styling and hex lookup tables
            ├── win_console.rs   # Windows console handling for --help and --tui
            └── tui/             # [tui] Terminal UI (mod.rs: state and input, view.rs: rendering)
```
