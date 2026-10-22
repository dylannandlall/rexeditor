# RexEditor — Project Memory & AI Handover Guide (`MEMORY.md`)

> **IMPORTANT FOR AI AGENTS:**
> This file is the single source of truth for `rexeditor`'s architecture, completed milestones, implemented features, user preferences, build quirks, and upcoming roadmap. **Update this file after every user prompt** so that any AI agent or developer can read `MEMORY.md` and immediately get up to speed on the exact state of the project.

---

## 1. Project Overview

**`rexeditor`** (`c:\Users\dylan\Code\rexeditor`) is a high-performance desktop GUI hex editor and binary analysis workstation written in Rust (`edition = "2024"`). It evolved from the terminal-based [`rexedit`](file:///c:/Users/dylan/Code/rexedit) TUI editor into a two-crate workspace separating a headless, UI-agnostic binary engine (`rex-core`) from an immediate-mode OpenGL desktop GUI (`rex-gui` built on `eframe` / `egui` 0.31).

### Core Design Goals
1. **Zero-Stall Multi-GB File Handling:** Files $\ge 4\text{ MiB}$ are memory-mapped read-only via `memmap2::Mmap` and edited non-destructively in memory using a Piece Table (`ByteBuffer`), avoiding loading or cloning huge buffers into RAM.
2. **60+ FPS Virtualized Viewport & 0% Idle CPU:** Only visible rows in the scroll viewport are projected ($O(1)$ uncompressed row math; $O(\log R)$ binary search over `CollapsedRun`s when repeated-row compression is enabled) and rendered in a single `egui::Painter` pass. The GUI is strictly reactive and sleeps at 0% CPU when no background worker is active.
3. **Integrated Reverse-Engineering Workflow:** Multi-pattern SIMD search, live Data Inspector with inline value encoding, persistent JSON structure overlays, side-by-side split & byte diffing, Shannon entropy & byte histograms, external script plugins, and an interactive persistent Python 3 REPL bridge.

---

## 2. Workspace & File Structure

```text
rexeditor/
├── .gitignore                   # Ignores /target/ and *.old.exe
├── LICENSE                      # MIT License
├── Cargo.toml                   # Workspace manifest (rex-core, rex-gui) + release LTO profile
├── README.md                    # High-level overview & Windows / Kali Linux build instructions
├── MEMORY.md                    # THIS FILE — AI project memory, milestone tracker & architecture
└── crates/
    ├── rex-core/                # Headless binary editing & analysis engine (UI-agnostic)
    │   ├── Cargo.toml           # Dependencies: memmap2, memchr, regex, serde, serde_json, thiserror, libc (unix), windows-sys (windows)
    │   └── src/
    │       ├── lib.rs           # Public re-exports for rex-core
    │       ├── buffer.rs        # BaseStorage (InMemory / Mmap) + PieceTable (ByteBuffer), atomic save, O(1) dirty check, find_next_diff / find_prev_diff
    │       ├── document.rs      # Document state, O(1) row projection + sparse CollapsedRun table, Undo/Redo, Overlays (with automatic fork-on-Save-As), clipboard formatters/parsers
    │       ├── model.rs         # Selection, EditKind (Overwrite/Insert), EditColumn, Field, Overlay, Theme, ByteColorMode (8 palettes, defaulting to Plain white)
    │       ├── inspector.rs     # Endianness-aware decoder & encoder (u8..i64, f32, f64, ULEB128, SLEB128, Unix timestamps, UTF-8, UTF-16LE, hex, binary)
    │       ├── search.rs        # Background streaming search worker: SIMD exact (memchr::memmem), full-byte & nibble wildcards (DE ?? 4?), regex with 64 KiB chunk overlap, typed integers (u16/u32/u64 LE/BE), itext, utf16le, dec, bin
    │       ├── entropy.rs       # Background 256-bucket Shannon entropy & byte frequency histogram worker with min/max/peak_offset & bucket<->offset mapping
    │       ├── plugins.rs       # External script/executable inspector plugin discovery & timeout-bounded runner (.exe/.bat/.cmd/.ps1/.py on Windows; chmod +x on Unix)
    │       └── python.rs        # Persistent Python 3 analysis REPL bridge with background warm-up, stdin interrupt thread, multi-document snapshots, and live UpdateSelection sync
    │
    └── rex-gui/                 # Desktop GUI Workstation (eframe / egui 0.31)
        ├── Cargo.toml           # Binary target `rexeditor`
        └── src/
            ├── main.rs          # CLI argument parser (`rexeditor [FILEPATH]... [-o|--offset <HEX_OR_DEC>]`) & native eframe launch
            ├── app.rs           # RexEditorApp: multi-tab workspace, global shortcuts, top menus (File/Edit/View/Help), Search dock, Python console dock, Split/Diff panes, Jump/Notice/Help modals
            ├── hex_viewport.rs  # Virtualized single-pass painter Hex + ASCII viewport, responsive split-pane scaling, mode badge button, context menu
            ├── sidebar.rs       # Right sidebar (bounded width): Data Inspector (with copy/inline-edit & truncation), Overlay Field Editor, Entropy & Histogram dock
            └── theme.rs         # Zero-allocation 256-entry hex lookup tables, color conversion, and workstation dark theme (scroll animation disabled)
```

---

## 3. Completed Milestones & History

### ✅ Milestone 1 — Workspace Split, Piece-Table Engine (`rex-core`), and Virtualized GUI (`rex-gui`)
- Created the two-crate workspace (`crates/rex-core` and `crates/rex-gui`).
- Implemented `ByteBuffer` ([`buffer.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/buffer.rs)) combining `memmap2::Mmap` (for files $\ge 4\text{ MiB}$) with an in-memory piece table (`Vec<Piece>` + append-only `added` buffer) and fast FNV-1a 64-bit `content_identity` hashing (sampling head/mid/tail for files $>16\text{ MiB}$).
- Built the virtualized single-pass `egui::Painter` hex + ASCII viewport ([`hex_viewport.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/hex_viewport.rs)) using static 256-entry uppercase/lowercase hex string lookup tables ([`theme.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/theme.rs)).
- Configured Windows AArch64 GNU/LLVM toolchain (`stable-aarch64-pc-windows-gnullvm`) in [`rust-toolchain.toml`](file:///c:/Users/dylan/Code/rexeditor/rust-toolchain.toml) so `cargo build` works without requiring Visual Studio C++ Build Tools.

### ✅ Milestone 2 — Full Hex/ASCII Editing, Piece-Table Coalescing, Undo/Redo, Clipboard, Inspector & Early Bugfixes
- **Nibble & ASCII Editing:** Full support for Overwrite and Insert modes in both Hex and ASCII columns, including a visual pending-nibble indicator (`A_`) in Insert mode before the low nibble is typed.
- **Piece-Table Original-Piece Restoration:** Overwriting bytes with values identical to the underlying base storage restores `PieceSource::Original` and coalesces adjacent pieces so `is_dirty()` and `is_modified(offset)` remain accurate in $O(1)$ / $O(\log P)$ time after edits or Undo/Redo.
- **Multi-Format Copy/Paste & Context Menu:** Copy selection as Spaced Hex (`Ctrl+C`), Compact Hex, ASCII (`Ctrl+Shift+C`), C array, Rust array, or Python `bytes` literal; Paste Hex (`Ctrl+V`, stripping `0x` prefixes) or Paste ASCII (`Ctrl+Shift+V`); right-click context menu in the hex viewport.
- **User-Reported Bugfixes Resolved in Milestone 2:**
  1. **Scroll Coasting Fixed:** Disabled `egui` exponential scroll smoothing (`i.smooth_scroll_delta = i.raw_scroll_delta` and `ScrollAnimation::none()`) so scrolling stops immediately where the user stops the wheel/trackpad.
  2. **Python Console First-Launch Delay Fixed:** Added `rex_core::python::warm_up()` on startup, cached Python executable discovery in a `OnceLock`, hid console windows with `CREATE_NO_WINDOW`, and moved `PythonSession::start` onto a background thread (`start_rx`) with command queueing so opening the Python console never blocks the UI thread.
  3. **Data Inspector Unbounded Width Fixed:** Capped the right sidebar width (`max_width = (available_width * 0.42).clamp(260.0, 440.0)`), truncated long inspector values with `…` (showing the full value on hover), and added per-row `📋` copy buttons and inline `Edit` buttons.

### ✅ Milestone 3 — High-Speed Search Engine, Entropy Visualization, Structure Overlays & CLI Arguments
- **CLI Positional File & `--offset` / `-o` Support ([`main.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/main.rs)):**
  - Supports `rexeditor <filepath> [-o|--offset <0xHEX_OR_DEC>]`.
  - If the requested offset exceeds the file size, clamps to EOF and displays a centered **Notice** popup modal warning the user that the offset does not exist.
- **Upgraded Search Engine ([`search.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/search.rs)):**
  - Supports exact hex, full-byte wildcards (`??`, `**`), **single-nibble wildcards** (`4?`, `?A`, `4*`, `*A`) with SIMD `memchr` anchor scanning, case-sensitive (`text:`) and case-insensitive (`itext:`) ASCII, `utf16le:`, typed integers (`u16le:`, `u16be:`, `u32le:`, `u32be:`, `u64le:`, `u64be:`), `dec:`, `bin:`, and chunked `re:` regex scanning with a `64 KiB` overlap window so boundary-spanning regex matches are never missed.
- **Sparse Repeated-Row Compression ([`document.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/document.rs)):**
  - Replaced dense `Vec<DisplayRow>` with a sparse `Vec<CollapsedRun>` table using $O(\text{runs})$ memory and $O(\log \text{runs})$ binary search (`partition_point`).
- **Interactive Entropy & Byte Histogram Dock ([`entropy.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/entropy.rs), [`sidebar.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/sidebar.rs)):**
  - Computes 256-bucket Shannon entropy ($0.0\text{–}8.0\text{ bits/byte}$), `min`, `max`, `peak_offset`, and 256-entry byte frequency histogram; clicking or dragging on the entropy curve jumps directly to that file offset; draws a live orange cursor marker on the curve.
- **Overlay Field Editor ([`sidebar.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/sidebar.rs)):**
  - Add (`a` key in View Mode), edit, color-code, jump to (`[` / `]`), and delete named byte-range fields with automatic JSON persistence.

### ✅ Milestone 4 — Side-by-Side Split Diffing, Live Python Selection Sync, and Inspector Plugins
- **Side-by-Side Split & Byte Diff Comparison ([`buffer.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/buffer.rs), [`app.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/app.rs)):**
  - Added `ByteBuffer::find_next_diff` and `ByteBuffer::find_prev_diff` (with wrap-around and length-mismatch handling).
  - Added Side-by-Side Split view (`◫ Split`, `Ctrl+B`), Byte Diff highlighting (`⚡ Diff`, `Ctrl+D`), synchronized scroll/cursor (`🔗 Sync`), comparison tab selector, and `◀ Diff` / `Diff ▶` toolbar buttons.
- **Live Python 3 Selection Sync ([`python.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/python.rs)):**
  - Added `PythonSession::update_selection` (`BridgeRequest::Select`) so changing the cursor or selection in the GUI updates `selected`, `selection_start`, `selection_end`, `selected_ranges`, and `buffer` in the running Python session without restarting the interpreter or losing user-defined REPL variables.
  - Added quick snippet buttons (`Help`, `Selection Hex`, `SHA-256`, `Entropy`) in the Python console toolbar.
- **Cross-Platform External Inspector Plugins ([`plugins.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/plugins.rs)):**
  - Discovers executable scripts in `.rexedit-plugins` / `~/.config/rexedit/plugins` (supporting `.exe`, `.bat`, `.cmd`, `.ps1`, and `.py` on Windows, and `chmod +x` files on Linux/Unix) and runs them on a background thread with a 1500 ms timeout when `r` is pressed.

### ✅ Post-Milestone 4 User Polish & Bugfixes (Current Version)
1. **`'i'` Keybind Cycles Edit Modes:** Pressing `i` (or clicking the mode badge in the viewport header) cycles `View Mode -> Edit (Overwrite) -> Edit (Insert) -> View Mode` ([`Document::cycle_edit_mode`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/document.rs#L483-L502)).
2. **Search Dock Toggle, Positioning & Hint Fit:**
   - Pressing `Ctrl+F` toggles the Search dock open and closed (even when the search `TextEdit` is focused), and added a `×` close button on the right of the Search dock.
   - Positioned the Search dock **above** the Python console tab in the bottom panel stack.
   - Dynamically sized the Search `TextEdit` width to fit the full example hint text (`"Hex (DE AD / DE ?? BE), text:hello, utf16:str, dec:256, bin:10101010, or re:[A-Z]+"`).
3. **Overlay Forking on Filename Change (`Save As` & Autosave):**
   - When a file is saved under a new filename (`Save As`), `Document::save_as` and `persist_automatic_overlay` fork a new `<new_filename>-overlay-<n>.json` linked to the new file while keeping the original overlay file untouched and linked to the original file ([`document.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/document.rs#L888-L931)).
4. **Default Theme Set to Regular White (`ByteColorMode::Plain`):**
   - `Theme::default()` opens with `ByteColorMode::Plain` (all-white regular hex/ASCII text) instead of `ByteClass`.
5. **Fixed `Ctrl+C` / `Ctrl+V` in Hex Viewport:**
   - Handled `egui::Event::Copy` and `egui::Event::Paste` (which `egui-winit` emits instead of raw `Key::C`/`Key::V`), synced with `arboard::Clipboard`, and cleared lingering `TextEdit` focus when clicking in the hex viewport.
6. **Fixed Split / Diff Mode Viewport Overlap:**
   - Replaced `ui.columns(2, ...)` with bounded `allocate_ui_with_layout` panes + strict per-pane clip rects and responsive font/cell scaling in [`hex_viewport.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/hex_viewport.rs#L120-L176) so the second pane's offsets never overlap the first pane's ASCII column.
7. **Global Keybindings & Scrollable `Help` (`Ctrl+H`) Menu:**
   - Assigned global shortcuts: `Ctrl+O` (Open), `Ctrl+S` (Save), `Ctrl+Shift+S` (Save As), `Ctrl+Shift+O` (Load Overlay JSON), `Ctrl+Alt+S` (Save Overlay JSON As), `Ctrl+P` (Python Console), `Ctrl+B` (Split View), `Ctrl+D` (Diff View), `Ctrl+F` (Search), `Ctrl+G` (Jump to Offset), `Ctrl+H` (Help & Keybindings).
   - Added a `Help` menu dropdown next to `View` on the top-left menu bar that opens a scrollable `Help & Keybindings (Ctrl+H)` popup modal explaining all major features and shortcuts.
8. **Cross-Platform Compatibility (Windows + Kali Linux 2025.3):**
   - Enabled `"x11"` and `"wayland"` features on `eframe` in root [`Cargo.toml`](file:///c:/Users/dylan/Code/rexeditor/Cargo.toml) so `rexeditor` compiles for both Windows (`x86_64` / `aarch64`) and Linux (`x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu`).
   - Removed the Windows-only `rust-toolchain.toml` override so Linux and Windows hosts each use their native `stable` toolchain automatically.
   - Added `xdg-open` support on Unix/Linux in [`sidebar.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-gui/src/sidebar.rs) when opening the inspector plugins folder (`📂`), and added automatic `python3` invocation for executable `.py` plugin scripts without shebangs on Unix in [`plugins.rs`](file:///c:/Users/dylan/Code/rexeditor/crates/rex-core/src/plugins.rs).
   - Verified with `cargo check --workspace --target x86_64-unknown-linux-gnu` and `cargo check --workspace --target aarch64-unknown-linux-gnu` alongside Windows `cargo test --workspace`.
9. **Local Git Repository Initialized (`main` branch):**
   - Initialized `.git` in `c:\Users\dylan\Code\rexeditor` with `.gitignore` and `LICENSE` (MIT), ready to push to GitHub (`https://github.com/dylannandlall/rexeditor.git`).

---

## 4. Future Milestones to Undertake

> **Current Status:** Cross-platform Windows + Kali Linux compatibility is complete. Ready to proceed with **Milestone 5**, **Milestone 6 (Bundled CLI / TUI Mode)**, and **Milestone 7**.

### 🔜 Milestone 5 — Structure Templates, Bookmarks / Comments, & Advanced Binary Inspection
- **Built-in Binary Header / Structure Templates:**
  - One-click auto-overlay templates for common binary formats (PE/COFF headers, ELF32/ELF64 headers, Mach-O, ZIP/PNG/PDF magic headers) that populate `Document::fields` automatically when applied.
- **Bookmarks & Offset Comments:**
  - Quick bookmark toggling (`Ctrl+M` or `b` in View Mode) and per-offset analyst notes/comments persisted inside the document's overlay JSON without cluttering structure field ranges.
- **Selection Checksums / Hashes & Byte Transforms in Inspector:**
  - Fast CRC32, MD5, SHA-1, and SHA-256 calculation for the selected byte range directly in the right-hand Data Inspector or a dedicated Hash/Transform tool dialog (e.g., XOR / bit-rotate / Base64 encode-decode on selected bytes).

### 🔜 Milestone 6 — Packaged CLI Option for Integrated Terminal UI (`--tui` / `--cli`)
- **Integrated TUI Mode in `rexeditor` Binary:**
  - Add a CLI flag (e.g., `rexeditor --tui <filepath>` / `rexeditor --cli <filepath>`, plus automatic fallback if launched in a headless SSH/terminal session with no `DISPLAY`/`WAYLAND_DISPLAY` on Linux) that launches a full-featured Terminal UI (`ratatui` + `crossterm`) powered directly by the shared `rex-core` engine (`Document`, `ByteBuffer`, `SearchWorker`, `EntropyWorker`, `PythonSession`, `InspectorData`, and `Overlay`s).
  - On Windows (`windows_subsystem = "windows"`), dynamically attach/allocate a console (`AttachConsole(ATTACH_PARENT_PROCESS)` / `AllocConsole`) when `--tui`, `--help`, or CLI errors are invoked so a single `rexeditor` executable cleanly supports both native GUI mode (no stray console window on double-click) and full TUI mode in the terminal.

### 🔜 Milestone 7 — Session Restore, Offline Linux Vendoring & Release Polish
- **Offline / Air-Gapped Kali Linux Packaging:**
  - Optional `cargo vendor` workflow or WSL Linux binary build script so air-gapped Kali 2025.3 machines can either run a prebuilt Linux binary or build 100% offline with `--offline`.
- **Workspace Session Persistence:**
  - Optional restoration of recently opened files (`File -> Open Recent`), window geometry, column count (`bytes_per_row`), and color palette across restarts.
- **Large-File Stress Tests & Release Packaging:**
  - Automated benchmarks for multi-GB piece-table edits, search throughput, and binary release packaging.

---

## 5. Keybindings Quick Reference

| Keybind | Action |
| :--- | :--- |
| `Ctrl + O` | Open binary file |
| `Ctrl + S` | Save active binary file |
| `Ctrl + Shift + S` | Save active binary as new file (forks overlay to new filename) |
| `Ctrl + Shift + O` | Load external Overlay JSON |
| `Ctrl + Alt + S` | Save Overlay JSON As |
| `Ctrl + P` | Toggle Python 3 Analysis Console |
| `Ctrl + B` | Toggle Side-by-Side Split View (requires 2+ open tabs) |
| `Ctrl + D` | Toggle Byte Diff Highlighting in Split View (requires 2+ open tabs) |
| `Ctrl + F` | Toggle Search Dock open/closed |
| `Ctrl + G` | Toggle Jump to Offset modal |
| `Ctrl + H` | Toggle Help & Keybindings scrollable popup |
| `Ctrl + C` / `Ctrl + Shift + C` | Copy selection as spaced Hex / ASCII |
| `Ctrl + V` / `Ctrl + Shift + V` | Paste clipboard as Hex / ASCII |
| `Ctrl + Z` / `Ctrl + Y` | Undo / Redo byte edit |
| `i` | Cycle edit mode: `View -> Edit (Overwrite) -> Edit (Insert) -> View` |
| `Insert` / `Esc` | Toggle Overwrite/Insert / Return to View Mode |
| `Tab` | Switch active editing column between Hex and ASCII |
| `e` | Toggle Entropy & Byte Histogram dock (in View Mode) |
| `a` | Add Overlay Field for current selection (in View Mode) |
| `[` / `]` | Jump to previous / next Overlay Field (in View Mode) |
| `F3` / `Shift + F3` (or `n` / `N`) | Jump to next / previous search match |
| `r` | Run discovered external inspector plugins on selection (in View Mode) |

---

## 6. Build & Environment Notes for AI Agents

1. **PowerShell `PATH` Setup on Windows:**
   Always initialize `Path` in PowerShell tool calls so `cargo` and `rustup` are found:
   ```powershell
   $env:Path = [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + [Environment]::GetEnvironmentVariable('Path', 'Machine')
   ```
2. **Avoid `Access is denied (os error 5)` When User Has `rexeditor.exe` Open:**
   Windows locks running `.exe` files from being overwritten, but **allows renaming a running `.exe`**. Before running `cargo build --release`, always rotate the existing binary:
   ```powershell
   if (Test-Path 'target\release\rexeditor.exe') { Move-Item -Force 'target\release\rexeditor.exe' 'target\release\rexeditor.old.exe' -ErrorAction SilentlyContinue }
   cargo build --release
   ```
3. **Parallel Subagents:**
   The user likes utilizing subagents (`invoke_subagent`) in parallel on disjoint modules during milestone implementation, followed by a single unified `cargo test --workspace` and `cargo build --release` pass to avoid contending on the Cargo build lock.
