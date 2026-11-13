//! "Open in editor" for a task's worktree file: a closed list of editors, a path the daemon
//! checked, and no shell.
//!
//! The webview names a task, a worktree-relative path, a line and an editor id; never an
//! absolute path or a program. The shell asks the daemon (`task.file` with `stat`, through
//! the bridge's allowlist) where that file is: the daemon resolves it inside the task's
//! worktree by descriptor walk (no symlinks, no `.git`). Only then is it handed to:
//!
//! - VS Code / Cursor: their `vscode://file/<path>:<line>` / `cursor://file/…` URL handler,
//!   through the opener crate's Rust API (not the webview's opener capability, which stays
//!   github.com and localhost only). The path is percent-encoded segment by segment, slashes
//!   kept (Batch 0 spike S3: `vscode://file/w/a%20b.rs:12`).
//! - Zed, Sublime Text, Xcode: `/usr/bin/open -b <bundle id> -- <path>` (no line: their
//!   line-aware entry points are command-line tools, which this never runs).
//! - The default: `/usr/bin/open -t -- <path>`, the system's default *text* editor. Never a
//!   plain `open <path>`: that runs whatever app the file type names (a `.command` runs in
//!   Terminal, a `.terminal` or `.webloc` opens what it says), and the file was written by an
//!   agent.
//!
//! `/usr/bin/open` runs with a cleared environment (PATH only). `$EDITOR` is never read: it
//! is a terminal program and could be anything.
//!
//! The daemon's check and the open are two steps: the agent could swap the file between
//! them. Right before anything is launched, the path is looked at again
//! ([`recheck_file`]): still a regular file, reached through no symlink. A folder is never
//! handed over: Xcode opens a `.xcodeproj` folder as a project, Zed or Sublime a folder
//! as a workspace, with whatever settings and tasks the agent put in it.
//!
//! VS Code and Cursor open a `vscode://file/…` path in a window; a file is not a trusted
//! folder by that, so their Workspace Trust (Restricted Mode for a folder they do not
//! know yet: no tasks, no debug configurations, no workspace settings that run code) still
//! applies when the person later opens the worktree as a folder. Keep
//! `security.workspace.trust.enabled` on: Mirante cannot check it.
use std::path::{Path, PathBuf};

/// An editor the person may pick (the webview sends its id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Editor {
    Vscode,
    Cursor,
    Zed,
    Sublime,
    Xcode,
    Default,
}

const ALL: [Editor; 6] = [Editor::Cursor, Editor::Vscode, Editor::Zed, Editor::Sublime, Editor::Xcode, Editor::Default];

impl Editor {
    pub fn parse(id: &str) -> Option<Editor> {
        Some(match id {
            "vscode" => Editor::Vscode,
            "cursor" => Editor::Cursor,
            "zed" => Editor::Zed,
            "sublime" => Editor::Sublime,
            "xcode" => Editor::Xcode,
            "default" => Editor::Default,
            _ => return None,
        })
    }

    /// The app bundle names it is installed as (in /Applications or ~/Applications).
    fn apps(self) -> &'static [&'static str] {
        match self {
            Editor::Vscode => &["Visual Studio Code.app"],
            Editor::Cursor => &["Cursor.app"],
            Editor::Zed => &["Zed.app"],
            Editor::Sublime => &["Sublime Text.app"],
            Editor::Xcode => &["Xcode.app"],
            Editor::Default => &[],
        }
    }
}

/// How a file is handed to an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// A URL for the editor's registered handler.
    Url(String),
    /// Arguments to `/usr/bin/open`.
    Open(Vec<String>),
}

/// Bytes kept as they are in a URL path segment (RFC 3986 unreserved); the rest is `%XX`.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An absolute path with no `.`/`..` level, no NUL, no line break: what the daemon returns
/// for a checked file. Anything else never reaches an editor.
pub fn checked_abs(abs: &str) -> Result<PathBuf, String> {
    let p = Path::new(abs);
    if !p.is_absolute() || abs.contains(['\0', '\n', '\r']) {
        return Err(format!("{abs:?} is not an absolute file path"));
    }
    if abs.split('/').skip(1).any(|s| s.is_empty() || s == "." || s == "..") {
        return Err(format!("{abs:?} has a relative level"));
    }
    Ok(p.to_path_buf())
}

/// A worktree-relative path as the webview sends it: plain levels only (the daemon checks
/// it again, by descriptor walk).
pub fn checked_rel(path: &str) -> Result<(), String> {
    let bad = path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.contains(['\0', '\\'])
        || path.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(format!("{path:?} is not a path inside the task's worktree"));
    }
    Ok(())
}

/// What to run to open `abs` (checked) at `line` in `editor`.
pub fn launch_for(editor: Editor, abs: &str, line: Option<u32>) -> Result<Launch, String> {
    let path = checked_abs(abs)?;
    let shown = path.to_str().ok_or("the path is not UTF-8")?;
    let at = line.filter(|l| *l > 0).map(|l| format!(":{l}")).unwrap_or_default();
    let url = |scheme: &str| {
        let encoded: Vec<String> = shown.split('/').map(encode_segment).collect();
        format!("{scheme}://file{}{at}", encoded.join("/"))
    };
    let open = |flag: &[&str]| {
        let mut args: Vec<String> = flag.iter().map(|s| s.to_string()).collect();
        args.extend(["--".to_string(), shown.to_string()]);
        Launch::Open(args)
    };
    Ok(match editor {
        Editor::Vscode => Launch::Url(url("vscode")),
        Editor::Cursor => Launch::Url(url("cursor")),
        Editor::Zed => open(&["-b", "dev.zed.Zed"]),
        Editor::Sublime => open(&["-b", "com.sublimetext.4"]),
        Editor::Xcode => open(&["-b", "com.apple.dt.Xcode"]),
        Editor::Default => open(&["-t"]),
    })
}

/// The whole decision, given how to ask the daemon for the file's checked absolute path
/// (`stat`: task, relative path → abs). The editor and the path are checked before the
/// daemon is asked; its answer is checked again before anything is launched. Returns the
/// launch and the file's path, for [`recheck_file`] right before the launch.
pub fn plan(
    editor: &str,
    task: &str,
    path: &str,
    line: Option<u32>,
    stat: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Result<(Launch, PathBuf), String> {
    let editor = Editor::parse(editor).ok_or_else(|| format!("unknown editor {editor:?}"))?;
    if task.is_empty() || task.len() > 200 || task.chars().any(char::is_control) {
        return Err("no such task".into());
    }
    checked_rel(path)?;
    let abs = stat(task, path)?;
    let launch = launch_for(editor, &abs, line)?;
    Ok((launch, checked_abs(&abs)?))
}

/// Right before the launch: `abs` is still a regular file and no level of it is a symlink
/// (its canonical path is itself). A folder, a link, a missing file: refused.
pub fn recheck_file(abs: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(abs).map_err(|e| format!("{}: {e}", abs.display()))?;
    if !m.file_type().is_file() {
        return Err(format!("{} is not a regular file any more: not opened", abs.display()));
    }
    match std::fs::canonicalize(abs) {
        Ok(c) if c == abs => Ok(()),
        _ => Err(format!("{} is reached through a symlink now: not opened", abs.display())),
    }
}

/// The editors installed here, in a stable order; the default text editor always.
pub fn installed(home: Option<&Path>) -> Vec<Editor> {
    let dirs: Vec<PathBuf> =
        [Some(PathBuf::from("/Applications")), home.map(|h| h.join("Applications"))].into_iter().flatten().collect();
    ALL.into_iter()
        .filter(|e| {
            *e == Editor::Default
                || e.apps()
                    .iter()
                    .any(|app| dirs.iter().any(|d| std::fs::symlink_metadata(d.join(app)).is_ok_and(|m| m.is_dir())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_url_is_built_from_the_daemon_path_only() {
        assert_eq!(
            launch_for(Editor::Cursor, "/w/a b.rs", Some(12)).unwrap(),
            Launch::Url("cursor://file/w/a%20b.rs:12".into())
        );
        assert_eq!(
            launch_for(Editor::Vscode, "/w/x:y#z?.rs", None).unwrap(),
            Launch::Url("vscode://file/w/x%3Ay%23z%3F.rs".into())
        );
        assert_eq!(
            launch_for(Editor::Vscode, "/w/é.rs", Some(0)).unwrap(),
            Launch::Url("vscode://file/w/%C3%A9.rs".into())
        );
        assert_eq!(
            launch_for(Editor::Default, "/w/-rf.sh", Some(3)).unwrap(),
            Launch::Open(vec!["-t".into(), "--".into(), "/w/-rf.sh".into()])
        );
        assert_eq!(
            launch_for(Editor::Zed, "/w/a.rs", Some(3)).unwrap(),
            Launch::Open(vec!["-b".into(), "dev.zed.Zed".into(), "--".into(), "/w/a.rs".into()])
        );
        for bad in ["relative/a.rs", "/w/../etc/passwd", "/w/./a", "/w/a\nb", ""] {
            assert!(launch_for(Editor::Vscode, bad, None).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn unknown_editor_is_refused() {
        for e in ["vim", "emacs", "$EDITOR", "VSCODE", "", "/usr/bin/open", "code --goto"] {
            let r = plan(e, "t1", "a.rs", None, |_, _| panic!("the daemon is never asked"));
            assert!(r.unwrap_err().contains("unknown editor"), "{e}");
        }
    }

    #[test]
    fn a_relative_or_dotdot_path_never_reaches_the_opener() {
        for bad in ["/etc/passwd", "../x", "a/../../b", "a//b", "./a", "a\\b", ""] {
            let r = plan("vscode", "t1", bad, None, |_, _| panic!("the daemon is never asked for {bad:?}"));
            assert!(r.is_err(), "{bad:?}");
        }
        // The daemon's answer is used, and checked again.
        let mut asked = Vec::new();
        let r = plan("vscode", "t1", "src/a.rs", Some(4), |t, p| {
            asked.push((t.to_string(), p.to_string()));
            Ok("/wt/src/a.rs".into())
        });
        assert_eq!(r.unwrap().0, Launch::Url("vscode://file/wt/src/a.rs:4".into()));
        assert_eq!(asked, [("t1".to_string(), "src/a.rs".to_string())]);
        assert!(plan("vscode", "t1", "a.rs", None, |_, _| Ok("a.rs".into())).is_err());
        assert!(plan("vscode", "t1", "a.rs", None, |_, _| Err("no task t1".into())).unwrap_err().contains("no task"));
        assert!(plan("vscode", "t\n1", "a.rs", None, |_, _| panic!("never asked")).is_err());
    }

    /// Review M6: what the daemon checked is looked at again right before the launch.
    #[test]
    fn a_file_swapped_after_the_check_is_not_opened() {
        let dir = std::env::temp_dir().join(format!("mirante-recheck-{}", std::process::id()));
        let dir = {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::canonicalize(&dir).unwrap()
        };
        let file = dir.join("a.rs");
        std::fs::write(&file, "x").unwrap();
        assert!(recheck_file(&file).is_ok());
        // Swapped for a folder (an Xcode project, a workspace).
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(&file).unwrap();
        assert!(recheck_file(&file).unwrap_err().contains("not a regular file"));
        std::fs::remove_dir(&file).unwrap();
        // Swapped for a symlink, or reached through one.
        std::fs::write(dir.join("real.rs"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("real.rs"), &file).unwrap();
        assert!(recheck_file(&file).is_err());
        std::os::unix::fs::symlink(&dir, dir.join("via")).unwrap();
        assert!(recheck_file(&dir.join("via/real.rs")).unwrap_err().contains("symlink"));
        assert!(recheck_file(&dir.join("gone.rs")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn installed_editors_are_found_by_their_app_bundle_and_default_is_always_there() {
        let home = std::env::temp_dir().join(format!("mirante-editors-{}", std::process::id()));
        std::fs::create_dir_all(home.join("Applications/Zed.app")).unwrap();
        // A plain file named like an app is no app.
        std::fs::write(home.join("Applications/Cursor.app"), "x").unwrap();
        let found = installed(Some(&home));
        assert!(found.contains(&Editor::Zed));
        assert!(!found.contains(&Editor::Cursor) || Path::new("/Applications/Cursor.app").is_dir());
        assert_eq!(found.last(), Some(&Editor::Default));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
