//! Console handling for Windows release builds, which use the Windows subsystem and
//! therefore start without a console.

use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, FreeConsole,
};

/// Attaches to the console of the launching shell so `--help` and error messages are
/// visible. Harmless when there is no parent console or one is already attached.
pub fn attach_parent() {
    // SAFETY: plain Win32 calls with no pointers; failure just leaves output unseen.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Gives the process a fresh console window for the interactive terminal UI.
///
/// A Windows-subsystem process is not waited on by `cmd.exe` or PowerShell, so sharing
/// the parent's console would leave the shell and the TUI fighting over keyboard input.
/// Debug builds and TUI-only builds are console apps and never call this.
#[allow(dead_code)]
pub fn alloc_new() {
    // SAFETY: plain Win32 calls with no pointers.
    unsafe {
        FreeConsole();
        AllocConsole();
    }
}
