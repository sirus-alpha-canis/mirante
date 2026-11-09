//! `mirante app`: find the desktop app's bundle and open it with macOS's `open` (never
//! through a shell).
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A `Mirante.app` to open instead of searching.
pub const APP_ENV: &str = "MIRANTE_APP";

/// Test seam: the `open` to run instead of `/usr/bin/open` (tests point it at a script so
/// no window opens).
pub const OPEN_BIN_ENV: &str = "MIRANTE_OPEN_BIN";

const OPEN_BIN: &str = "/usr/bin/open";
const BUNDLE: &str = "Mirante.app";

/// Where `tauri build --bundles app` leaves the bundle, from the checkout's root.
pub const BUILD_OUTPUT: &str = "apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app";

/// What `daemon start` / `daemon restart` add on a terminal.
pub const HINT: &str = "open the cockpit with: mirante app";

/// Said off macOS.
pub const MACOS_ONLY: &str = "the desktop app is macOS-only for now; on this system use the CLI: \
mirante watch (live board), mirante attach <task> (live terminal), mirante help";

/// The places the bundle is looked for, from this process's environment.
#[derive(Debug, Clone)]
pub struct Where {
    /// `MIRANTE_APP`, when set and not empty.
    pub app_env: Option<PathBuf>,
    /// `/Applications` (a parameter for tests).
    pub applications: PathBuf,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// This executable (the `~/.local/bin` link is followed to the checkout's build).
    pub exe: Option<PathBuf>,
}

impl Where {
    pub fn from_env() -> Where {
        Where {
            app_env: non_empty(std::env::var_os(APP_ENV)),
            applications: PathBuf::from("/Applications"),
            home: non_empty(std::env::var_os("HOME")),
            exe: std::env::current_exe().ok(),
        }
    }
}

/// A directory whose name ends in `.app`.
pub fn is_bundle(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "app") && p.is_dir()
}

/// The checkout's build output for the `mirante` at `exe` (links followed):
/// `<root>/target/release/mirante` → `<root>/apps/cockpit/…/Mirante.app`.
pub fn build_output(exe: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(exe).ok()?;
    let root = real.parent()?.parent()?.parent()?;
    Some(root.join(BUILD_OUTPUT))
}

/// The searched places, in order (`MIRANTE_APP` aside).
pub fn candidates(w: &Where) -> Vec<PathBuf> {
    let mut out = vec![w.applications.join(BUNDLE)];
    if let Some(home) = &w.home {
        out.push(home.join("Applications").join(BUNDLE));
    }
    if let Some(built) = w.exe.as_deref().and_then(build_output) {
        out.push(built);
    }
    out
}

/// The bundle to open: `MIRANTE_APP` (which must be one), else the first candidate that is.
pub fn resolve(w: &Where) -> Result<PathBuf, String> {
    if let Some(p) = &w.app_env {
        return if is_bundle(p) {
            Ok(p.clone())
        } else {
            Err(format!("{APP_ENV}={} is not an app bundle (a directory whose name ends in .app)", p.display()))
        };
    }
    let looked = candidates(w);
    if let Some(found) = looked.iter().find(|p| is_bundle(p)) {
        return Ok(found.clone());
    }
    let list: Vec<String> = looked.iter().map(|p| format!("  {}", p.display())).collect();
    Err(format!(
        "the desktop app is not built or installed; looked in:\n{}\n\
         build it from your Mirante checkout with `scripts/install.sh --app` (add --install-app to \
         copy it to /Applications), or by hand: `pnpm --dir apps/cockpit install && \
         pnpm --dir apps/cockpit tauri build --bundles app` (Node 22.12+, pnpm 10); or set \
         {APP_ENV} to a Mirante.app",
        list.join("\n")
    ))
}

/// `open -a <bundle>`, or `MIRANTE_OPEN_BIN` in tests.
pub fn open_command(bundle: &Path) -> Command {
    let bin = non_empty(std::env::var_os(OPEN_BIN_ENV)).unwrap_or_else(|| PathBuf::from(OPEN_BIN));
    let mut c = Command::new(bin);
    c.arg("-a").arg(bundle);
    c
}

/// The hint after `daemon start|restart`, only when stdout is a terminal.
pub fn daemon_hint(stdout_is_tty: bool) -> Option<&'static str> {
    stdout_is_tty.then_some(HINT)
}

fn non_empty(v: Option<OsString>) -> Option<PathBuf> {
    v.filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    fn mkapp(p: &Path) -> PathBuf {
        std::fs::create_dir_all(p).unwrap();
        p.to_path_buf()
    }

    fn same(a: &Path, b: &Path) -> bool {
        std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
    }

    /// Every place empty: `<d>/Applications`, home `<d>/home`, exe `<d>/bin/mirante` (absent).
    fn nowhere(d: &TestDir) -> Where {
        Where {
            app_env: None,
            applications: d.path().join("Applications"),
            home: Some(d.path().join("home")),
            exe: Some(d.path().join("bin/mirante")),
        }
    }

    /// A checkout at `<d>/ckout` with a release `mirante`, linked from `<d>/bin/mirante`.
    fn checkout(d: &TestDir) -> PathBuf {
        let root = d.path().join("ckout");
        std::fs::create_dir_all(root.join("target/release")).unwrap();
        std::fs::write(root.join("target/release/mirante"), "").unwrap();
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::os::unix::fs::symlink(root.join("target/release/mirante"), d.path().join("bin/mirante")).unwrap();
        root
    }

    #[test]
    fn the_search_order_is_applications_then_home_then_the_checkouts_build() {
        let d = TestDir::new();
        let w = nowhere(&d);
        let root = checkout(&d);
        let built = mkapp(&root.join(BUILD_OUTPUT));
        assert!(same(&resolve(&w).unwrap(), &built));
        let mine = mkapp(&d.path().join("home/Applications/Mirante.app"));
        assert!(same(&resolve(&w).unwrap(), &mine));
        let system = mkapp(&d.path().join("Applications/Mirante.app"));
        assert!(same(&resolve(&w).unwrap(), &system));
        let looked = candidates(&w);
        assert_eq!(looked.len(), 3, "{looked:?}");
        assert!(same(&looked[2], &built), "{looked:?}");
    }

    #[test]
    fn mirante_app_wins_and_must_be_a_bundle() {
        let d = TestDir::new();
        let mut w = nowhere(&d);
        mkapp(&d.path().join("Applications/Mirante.app"));
        let other = mkapp(&d.path().join("elsewhere/Dev Mirante.app"));
        w.app_env = Some(other.clone());
        assert_eq!(resolve(&w).unwrap(), other);
        // Set but wrong: said, not silently replaced by another copy.
        for bad in [d.path().join("elsewhere"), d.path().join("missing.app")] {
            w.app_env = Some(bad.clone());
            let e = resolve(&w).unwrap_err();
            assert!(e.contains("MIRANTE_APP") && e.contains(&bad.display().to_string()), "{e}");
        }
    }

    #[test]
    fn a_bundle_is_a_directory_ending_in_dot_app() {
        let d = TestDir::new();
        assert!(is_bundle(&mkapp(&d.path().join("A.app"))));
        assert!(!is_bundle(&mkapp(&d.path().join("A"))));
        std::fs::write(d.path().join("F.app"), "").unwrap();
        assert!(!is_bundle(&d.path().join("F.app")));
        assert!(!is_bundle(&d.path().join("missing.app")));
        // A file where the bundle should be is skipped, not opened.
        let w = nowhere(&d);
        std::fs::create_dir_all(&w.applications).unwrap();
        std::fs::write(w.applications.join("Mirante.app"), "").unwrap();
        assert!(resolve(&w).is_err());
    }

    #[test]
    fn the_build_output_follows_the_link_to_the_checkout() {
        let d = TestDir::new();
        let root = checkout(&d);
        let out = build_output(&d.path().join("bin/mirante")).unwrap();
        assert!(out.ends_with(BUILD_OUTPUT), "{out:?}");
        assert_eq!(out, std::fs::canonicalize(&root).unwrap().join(BUILD_OUTPUT));
        assert_eq!(build_output(&d.path().join("missing")), None);
    }

    #[test]
    fn nothing_found_says_where_it_looked_and_how_to_build() {
        let d = TestDir::new();
        let w = nowhere(&d);
        let e = resolve(&w).unwrap_err();
        assert!(e.contains("not built or installed"), "{e}");
        assert!(e.contains(&w.applications.join("Mirante.app").display().to_string()), "{e}");
        assert!(e.contains("scripts/install.sh --app"), "{e}");
        assert!(e.contains("pnpm --dir apps/cockpit tauri build --bundles app"), "{e}");
        assert!(e.contains("MIRANTE_APP"), "{e}");
    }

    #[test]
    fn open_runs_open_dash_a_on_the_bundle_without_a_shell() {
        let c = open_command(Path::new("/Applications/Mirante.app"));
        if std::env::var_os(OPEN_BIN_ENV).is_none() {
            assert_eq!(c.get_program(), "/usr/bin/open");
        }
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["-a", "/Applications/Mirante.app"]);
    }

    #[test]
    fn the_hint_is_only_for_a_terminal() {
        assert_eq!(daemon_hint(true), Some("open the cockpit with: mirante app"));
        assert_eq!(daemon_hint(false), None);
    }
}
