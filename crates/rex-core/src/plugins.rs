//! User-defined Inspector rows.
//!
//! Dropping an executable script into the plugin directory (see
//! [`default_directory`]) adds one more row to the Inspector pane, without
//! recompiling rexedit. Plugins only run when the user presses `r`
//! (`App::run_inspector_plugins`), never automatically on a selection
//! change — even a handful of slow (e.g. Python-based) scripts firing on
//! every mouse drag or held arrow key adds up. On `r`, every discovered
//! plugin runs on a background thread with the selected bytes (capped at
//! [`MAX_INPUT_BYTES`]) written to its stdin; the first line of its stdout
//! becomes the row's value. A plugin that exits non-zero, times out, or
//! prints nothing shows [`ERROR_PLACEHOLDER`] instead of being silently
//! dropped, so a broken script is visible rather than missing.
//!
//! To turn the feature off, delete the scripts (or the whole directory);
//! nothing else about rexedit depends on it.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Bytes beyond this are neither hashed to detect a selection change nor
/// sent to a plugin, mirroring the Inspector's other bounded previews (see
/// `utf8_preview` in `ui.rs`).
pub const MAX_INPUT_BYTES: usize = 4096;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Shown in place of a plugin's value when it fails in any way.
pub const ERROR_PLACEHOLDER: &str = "(error)";
/// Shown for a plugin that's part of a run still in progress but hasn't
/// reported back yet.
pub const PENDING_PLACEHOLDER: &str = "(running…)";
/// Shown for a plugin that has never been run this session (plugins only
/// run when the user asks, via `r`).
pub const NOT_RUN_PLACEHOLDER: &str = "(press r to run)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    path: PathBuf,
}

impl Plugin {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub struct PluginOutcome {
    pub name: String,
    pub value: String,
}

pub struct PluginWorker {
    pub receiver: Receiver<PluginOutcome>,
    cancel: Arc<AtomicBool>,
}

impl PluginWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Discovers plugins: executable files directly inside `directory`,
/// labeled after their file name with the extension removed and sorted by
/// that name. A missing directory simply yields no plugins.
pub fn discover(directory: &Path) -> Vec<Plugin> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut plugins = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_executable(path))
        .map(|path| Plugin {
            name: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            path,
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("exe" | "bat" | "cmd" | "ps1" | "py")
    )
}

/// Runs every plugin against `bytes` on a background thread, streaming one
/// [`PluginOutcome`] back per plugin as it finishes, so a slow or hung
/// script never blocks the UI. Call [`PluginWorker::cancel`] when a newer
/// selection supersedes this run; already-finished outcomes still arrive,
/// but no further plugins are started.
pub fn spawn(plugins: Vec<Plugin>, bytes: Vec<u8>) -> PluginWorker {
    spawn_with_timeout(plugins, bytes, DEFAULT_TIMEOUT)
}

fn spawn_with_timeout(plugins: Vec<Plugin>, bytes: Vec<u8>, timeout: Duration) -> PluginWorker {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        for plugin in plugins {
            if worker_cancel.load(Ordering::Relaxed) {
                return;
            }
            let value = run_plugin(&plugin.path, &bytes, timeout)
                .unwrap_or_else(|_| ERROR_PLACEHOLDER.to_string());
            if sender
                .send(PluginOutcome {
                    name: plugin.name,
                    value,
                })
                .is_err()
            {
                return;
            }
        }
    });
    PluginWorker { receiver, cancel }
}

fn run_plugin(path: &Path, bytes: &[u8], timeout: Duration) -> Result<String, String> {
    let input = &bytes[..bytes.len().min(MAX_INPUT_BYTES)];
    let mut child = build_command(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;

    // `input` is capped well below typical pipe buffer sizes, so writing it
    // before reading stdout back cannot deadlock a script that does not
    // read stdin at all.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "plugin did not accept input".to_string())?;
    let write_result = stdin.write_all(input);
    drop(stdin);
    write_result.map_err(|error| error.to_string())?;

    let status = wait_with_timeout(&mut child, timeout)?;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    if !status.success() {
        return Err(format!("exit {}", status.code().unwrap_or(-1)));
    }
    let line = output.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Err("no output".into());
    }
    Ok(line.to_string())
}

fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => return Ok(status),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
        }
    }
}

#[cfg(windows)]
fn build_command(path: &Path) -> Command {
    let ext = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|s| s.to_ascii_lowercase());
    let mut command = match ext.as_deref() {
        Some("bat" | "cmd") => {
            let mut command = Command::new("cmd");
            command.args(["/C", &path.to_string_lossy()]);
            command
        }
        Some("ps1") => {
            let mut command = Command::new("powershell");
            command.args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &path.to_string_lossy(),
            ]);
            command
        }
        Some("py") => {
            let python = std::env::var("PYTHON").unwrap_or_else(|_| "python".to_string());
            let mut command = Command::new(python);
            command.arg(path);
            command
        }
        _ => Command::new(path),
    };
    command.creation_flags(0x0800_0000);
    command
}

#[cfg(not(windows))]
fn build_command(path: &Path) -> Command {
    let is_py = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("py"));
    if is_py {
        let has_shebang = fs::read(path).map(|b| b.starts_with(b"#!")).unwrap_or(false);
        if !has_shebang {
            let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
            let mut command = Command::new(python);
            command.arg(path);
            return command;
        }
    }
    Command::new(path)
}

/// `<data dir>/rexedit/inspectors`, matching where overlays are stored.
pub fn default_directory() -> PathBuf {
    crate::model::rexedit_data_dir("inspectors")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_script(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rexedit-inspector-plugin-test-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cross_platform_plugin_discover_and_run() {
        let directory = temp_dir("cross-platform");
        #[cfg(windows)]
        let script_path = {
            let p = directory.join("hello_plugin.bat");
            fs::write(&p, "@echo off\r\necho plugin-ok\r\n").unwrap();
            p
        };
        #[cfg(unix)]
        let script_path = write_script(&directory, "hello_plugin.sh", "#!/bin/sh\necho plugin-ok\n");

        // Non-executable file should be ignored
        fs::write(directory.join("readme.txt"), "ignore me").unwrap();

        let plugins = discover(&directory);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "hello_plugin");
        assert_eq!(plugins[0].path(), script_path.as_path());

        let worker = spawn_with_timeout(plugins, vec![0xDE, 0xAD, 0xBE, 0xEF], Duration::from_secs(2));
        let outcome = worker
            .receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert_eq!(outcome.name, "hello_plugin");
        assert_eq!(outcome.value, "plugin-ok");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[cfg(unix)]
    fn discovers_only_executable_files_sorted_by_name() {
        let directory = temp_dir("discover");
        write_script(&directory, "zebra.sh", "#!/bin/sh\necho z\n");
        write_script(&directory, "alpha.sh", "#!/bin/sh\necho a\n");
        fs::write(directory.join("not-executable.sh"), "#!/bin/sh\necho n\n").unwrap();

        let plugins = discover(&directory);
        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zebra"]
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn discovering_a_missing_directory_yields_no_plugins() {
        let directory = std::env::temp_dir().join("rexedit-inspector-plugin-test-missing");
        let _ = fs::remove_dir_all(&directory);
        assert_eq!(discover(&directory), Vec::new());
    }

    #[test]
    #[cfg(unix)]
    fn runs_a_plugin_against_the_selected_bytes() {
        let directory = temp_dir("run");
        write_script(
            &directory,
            "sum.sh",
            "#!/bin/sh\nod -An -tu1 -v | tr -s ' \\n' '+' | sed 's/^+//; s/+$//' | tr -d '\\n'; echo\n",
        );
        let plugins = discover(&directory);
        assert_eq!(plugins.len(), 1);

        let worker = spawn_with_timeout(plugins, vec![1, 2, 3], Duration::from_millis(500));
        let outcome = worker
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(outcome.name, "sum");
        assert_eq!(outcome.value, "1+2+3");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[cfg(unix)]
    fn a_failing_plugin_reports_the_error_placeholder() {
        let directory = temp_dir("fail");
        write_script(&directory, "broken.sh", "#!/bin/sh\nexit 1\n");
        let plugins = discover(&directory);

        let worker = spawn_with_timeout(plugins, vec![0], Duration::from_millis(500));
        let outcome = worker
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(outcome.value, ERROR_PLACEHOLDER);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[cfg(unix)]
    fn a_hanging_plugin_times_out_with_the_error_placeholder() {
        let directory = temp_dir("hang");
        write_script(&directory, "hang.sh", "#!/bin/sh\nsleep 5\necho late\n");
        let plugins = discover(&directory);

        let worker = spawn_with_timeout(plugins, vec![0], Duration::from_millis(100));
        let outcome = worker
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(outcome.value, ERROR_PLACEHOLDER);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[cfg(unix)]
    fn cancelling_a_worker_stops_remaining_plugins() {
        let directory = temp_dir("cancel");
        write_script(
            &directory,
            "a_first.sh",
            "#!/bin/sh\nsleep 0.2\necho first\n",
        );
        write_script(&directory, "b_second.sh", "#!/bin/sh\necho second\n");
        let plugins = discover(&directory);
        assert_eq!(plugins.len(), 2);

        let worker = spawn_with_timeout(plugins, vec![0], Duration::from_millis(500));
        worker.cancel();
        // The in-flight first plugin may still report its outcome, but the
        // second must never run once cancellation is observed between
        // plugins.
        let mut names = Vec::new();
        while let Ok(outcome) = worker.receiver.recv_timeout(Duration::from_millis(400)) {
            names.push(outcome.name);
        }
        assert!(!names.contains(&"second".to_string()));
        let _ = fs::remove_dir_all(&directory);
    }
}
