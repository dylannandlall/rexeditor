//! File-system path helpers shared by the TUI prompts and the GUI file picker.

use std::path::PathBuf;

/// Expands a leading `~` to the home directory; other paths are returned as-is.
pub fn expand_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/'))
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest.trim_start_matches('/'));
    }
    PathBuf::from(input)
}

/// Result of completing a path the way a shell does.
#[derive(Debug, PartialEq, Eq)]
pub struct PathCompletion {
    /// The input with the longest common prefix of all matches filled in.
    pub text: String,
    /// Names of every match (directories end in `/`), sorted.
    pub matches: Vec<String>,
}

/// Completes the last path component of `input` against the file system: `~` expands to the
/// home directory, directories get a trailing `/`, and dotfiles only match a `.` prefix.
pub fn complete_path(input: &str) -> PathCompletion {
    // A bare `~` completes to `~/`.
    if input == "~" {
        return PathCompletion {
            text: "~/".into(),
            matches: Vec::new(),
        };
    }
    let split = input.rfind('/').map_or(0, |i| i + 1);
    let (dir_part, name_part) = input.split_at(split);
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        expand_path(dir_part)
    };
    let unchanged = PathCompletion {
        text: input.to_string(),
        matches: Vec::new(),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return unchanged;
    };
    let mut matches: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let mut name = e.file_name().into_string().ok()?;
            if !name.starts_with(name_part)
                || (name.starts_with('.') && !name_part.starts_with('.'))
            {
                return None;
            }
            if e.path().is_dir() {
                name.push('/');
            }
            Some(name)
        })
        .collect();
    matches.sort();
    let Some(first) = matches.first() else {
        return unchanged;
    };
    let mut common = first.clone();
    for m in &matches[1..] {
        let n = common
            .chars()
            .zip(m.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        common.truncate(n);
    }
    PathCompletion {
        text: format!("{dir_part}{common}"),
        matches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_only_at_the_start() {
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return;
        };
        let home = PathBuf::from(home);
        assert_eq!(expand_path("~"), home);
        assert_eq!(expand_path("~/bins/a.bin"), home.join("bins/a.bin"));
        assert_eq!(expand_path("~other/x"), PathBuf::from("~other/x"));
        assert_eq!(expand_path("/tmp/~/x"), PathBuf::from("/tmp/~/x"));
        assert_eq!(expand_path("rel/x"), PathBuf::from("rel/x"));
    }
}
