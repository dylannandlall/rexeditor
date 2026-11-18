# RexEditor — Cross-Platform Binary Analysis Workstation

`rexeditor` is a high-performance Windows & Linux (Kali 2025.3) desktop hex editor and binary analysis workstation evolved from [`rexedit`](https://github.com/dylannandlall/rexedit).

It separates a UI-agnostic Rust binary engine (`crates/rex-core`) from the hardware-accelerated desktop workstation frontend (`crates/rex-gui`), making it fast on older/lower-end hardware while scaling effortlessly to multi-gigabyte binaries without loading entire files into RAM.

---

## Workspace Architecture

```text
rexeditor/
├── Cargo.toml                   # Workspace manifest
└── crates/
    ├── rex-core/                # Headless binary engine (UI-agnostic, TUI & GUI ready)
    │   └── src/
    │       ├── lib.rs
    │       ├── buffer.rs        # Read-only memmap2::Mmap base + in-memory PieceTable
    │       ├── document.rs      # O(1) virtual row projection, Undo/Redo, Overlays, Atomic Save
    │       ├── model.rs         # Selection, Field, Overlay, Theme, 8 ByteColorModes
    │       ├── inspector.rs     # LE/BE integer, float, LEB128, UTF-8/UTF-16LE, Unix time decoder/encoder
    │       ├── search.rs        # SIMD (memchr::memmem) + masked wildcard + regex background search
    │       ├── entropy.rs       # 256-bucket Shannon entropy & byte frequency histogram worker
    │       ├── plugins.rs       # External script/executable inspector plugin runner
    │       └── python.rs        # Persistent Python 3 analysis REPL bridge
    │
    └── rex-gui/                 # `rexeditor` binary: desktop GUI (eframe / egui) + terminal UI (ratatui)
        └── src/
            ├── main.rs          # Entry point: picks GUI or TUI, Windows console handling
            ├── cli.rs           # Command-line parsing shared by both front ends
            ├── app.rs           # [gui] Multi-tab workspace, menus, shortcuts, Search/Python docks
            ├── hex_viewport.rs  # [gui] Virtualized single-pass painter Hex + ASCII viewport
            ├── sidebar.rs       # [gui] Data Inspector, Field Editor, and Entropy plot
            ├── theme.rs         # [gui] Zero-allocation hex lookup tables & workstation styling
            ├── win_console.rs   # Windows console attach/alloc for --help and --tui
            └── tui/             # [tui] Terminal UI (`rexeditor --tui`)
                ├── mod.rs       # State, key/mouse handling, prompts
                └── view.rs      # Hex view, inspector, field list, help rendering
```

---

## Key Engine Optimizations over `rexedit`

1. **`memmap2` + Piece Table (`ByteBuffer`):**
   - Files $\ge 4\text{ MiB}$ are memory-mapped read-only via the OS page cache rather than eagerly read and duplicated into `Vec<u8>`.
   - Overwrites, insertions, and deletions operate on an in-memory piece table (`Vec<Piece>` + append-only edit buffer) without shifting or cloning the underlying file buffer.
2. **$O(1)$ Uncompressed Row Projection:**
   - Uncompressed rows are calculated on the fly (`offset = row * bytes_per_row`) without allocating a `Vec<DisplayRow>` per row across multi-GB files.
   - `collapsed_rows` is only populated when **Compress Repeated Rows** is toggled on.
3. **Instant Piece-Table Dirty Tracking:**
   - Modified bytes are identified directly from `PieceSource::Added` spans in $O(\log P)$ time rather than scanning `0..bytes.len()` after each keystroke.
4. **SIMD-Accelerated Streaming Search:**
   - Literal hex (`DE AD BE EF`), decimal (`dec:`), binary (`bin:`), ASCII/UTF-8 (`text:`), and UTF-16LE (`utf16:`) queries use `memchr::memmem::Finder`, while wildcard hex queries (`DE ?? BE`) use `memchr` anchor scanning.
5. **Reactive Event-Driven GUI:**
   - The GUI sleeps at 0% CPU when idle and only requests periodic repaints while a background search, entropy scan, or Python command is actively streaming results.

---

## Terminal UI (`--tui`)

`rexeditor --tui [FILEPATH]...` runs the editor inside the terminal: no graphical display, X11 or
Wayland needed. It works over SSH and on server installs. A build that includes the GUI also falls
back to the terminal UI automatically on Linux when neither `DISPLAY` nor `WAYLAND_DISPLAY` is set.

The terminal UI uses the same engine and overlay files as the GUI: hex/ASCII view, overwrite and
insert editing with undo/redo, search (`/`), jump to offset (`:`), the Data Inspector, and overlay
fields (show/hide plus border, text and background colors). Mouse clicks, drag-selection and the
wheel work in terminals with mouse support. Copying (`y` / `Ctrl+C`) goes through the terminal's
clipboard (OSC 52), so it also works over SSH. Press `?` inside the TUI for all keybindings.

Entropy view, Python console, inspector plugins and split/diff view remain GUI-only for now.

### TUI-only build

To build without any GUI dependencies (eframe, winit, OpenGL, X11/Wayland), e.g. for a
server or minimal Linux install:

```bash
cargo build --release -p rex-gui --no-default-features --features tui
./target/release/rexeditor [FILEPATH]
```

The binary starts the terminal UI directly. The Cargo features are `gui` and `tui` (both on by
default); `--no-default-features --features gui` builds the GUI alone.

On Windows, release builds that include the GUI open the terminal UI in a new console window,
because Windows shells don't wait for GUI-subsystem programs and would compete for keyboard
input. Build the TUI-only binary to run it inside an existing terminal.

---

## Building and Running

### Windows
```powershell
cd C:\Users\dylan\Code\rexeditor
cargo test --workspace
cargo build --release --bin rexeditor
.\target\release\rexeditor.exe [--tui] [FILEPATH] [-o|--offset <0xHEX_OR_DEC>]
```

### Linux / Kali Linux 2025.3 (Online & Air-Gapped Offline)
`rexeditor` uses runtime `dlopen` for X11/Wayland/OpenGL (`winit`/`glutin`), pure-Rust XDG Desktop Portals (`rfd`/`ashpd`), and pure-Rust X11/Wayland clipboard handling (`arboard`), so **no `-dev` C header packages (`libgtk-3-dev`, `libx11-dev`, etc.) are required**.

- **Online Linux build:**
  ```bash
  cargo build --release --bin rexeditor
  ./target/release/rexeditor [--tui] [FILEPATH] [-o|--offset <0xHEX_OR_DEC>]
  ```
- **Air-Gapped / Offline Kali 2025.3 build:**
  1. On an internet-connected machine, vendor the dependencies into the repository:
     ```bash
     cargo vendor vendor
     ```
     And add to `.cargo/config.toml`:
     ```toml
     [source.crates-io]
     replace-with = "vendored-sources"

     [source.vendored-sources]
     directory = "vendor"
     ```
  2. Copy the repository folder to the air-gapped Kali machine and build offline:
     ```bash
     cargo build --release --offline --bin rexeditor
     ```
