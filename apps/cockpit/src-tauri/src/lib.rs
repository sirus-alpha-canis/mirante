//! Tauri shell of the cockpit: nine commands that forward to `mirante-bridge`, plus three
//! small path helpers for adding a project (the home directory, what is at a path, and the
//! git checkouts in a folder: see `scan`). No state of
//! its own beyond the running stream and open attachments; the daemon owns everything.
//! The window never leaves the app's origin: links open in the browser through the opener
//! plugin (github.com only, see `capabilities/default.json`), never in the window itself,
//! and never in a window of their own: `window.open` / `target=_blank` are denied.
mod scan;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use mirante_bridge::attach::{Attachments, Opened};
use mirante_bridge::stream::{self, Stream, StreamMsg};
use mirante_bridge::{rpc as bridge_rpc, BridgeError};
use mirante_ipc::types::DaemonBinary;
use mirante_ipc::Paths;
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::window::Color;
use tauri::{AppHandle, Manager, State, Theme, Url, WebviewWindowBuilder, WindowEvent};

/// Vite's dev server (`build.devUrl` in `tauri.conf.json`), loaded only by `tauri dev`.
const DEV_ORIGIN: (&str, u16) = ("127.0.0.1", 1420);

/// Whether the window may load `url`: the bundled app (`tauri://localhost` on macOS,
/// `http(s)://tauri.localhost` elsewhere), plus the dev server in a dev build.
fn is_app_url(url: &Url, dev: bool) -> bool {
    match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) => true,
        ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some(host)) => dev && host == DEV_ORIGIN.0 && url.port() == Some(DEV_ORIGIN.1),
        _ => false,
    }
}

/// The window's colour before the page paints: the app's `--panel` (`src/styles.css`, and the
/// launch splash in `index.html`) for the system's theme. macOS paints no webview background of
/// its own colour, hence the window also starts hidden (see `run`).
fn backdrop(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color(0x1b, 0x1c, 0x21, 0xff),
        _ => Color(0xf8, 0xf7, 0xf4, 0xff),
    }
}

/// The longest the window stays hidden waiting for its page (it shows on the first load).
const SHOW_AT_THE_LATEST: Duration = Duration::from_secs(3);

struct Cockpit {
    paths: Paths,
    socket: PathBuf,
    stream: Mutex<Option<Stream>>,
    attachments: Attachments,
}

/// Terminal output for the frontend: raw bytes (an `ArrayBuffer` in the webview, no
/// base64 or JSON on either side), then one JSON `{"type":"end"}` when the stream ends.
fn output_msg(bytes: Vec<u8>) -> InvokeResponseBody {
    InvokeResponseBody::Raw(bytes)
}

fn end_msg() -> InvokeResponseBody {
    InvokeResponseBody::Json(r#"{"type":"end"}"#.to_string())
}

fn joined<T>(r: Result<Result<T, BridgeError>, tauri::Error>) -> Result<T, BridgeError> {
    r.map_err(|e| BridgeError::Protocol { message: e.to_string() })?
}

#[tauri::command]
async fn rpc(app: AppHandle, method: String, params: Value) -> Result<Value, BridgeError> {
    let socket = app.state::<Cockpit>().socket.clone();
    joined(tauri::async_runtime::spawn_blocking(move || bridge_rpc::call(&socket, &method, params)).await)
}

/// (Re)starts the event stream into `on_msg`; a previous stream is stopped.
#[tauri::command]
fn subscribe(app: State<'_, Cockpit>, on_msg: Channel<StreamMsg>) {
    let s = stream::spawn(app.socket.clone(), move |m| {
        let _ = on_msg.send(m);
    });
    let old = app.stream.lock().unwrap().replace(s);
    if let Some(old) = old {
        std::thread::spawn(move || old.stop());
    }
}

#[tauri::command]
async fn attach_open(
    app: AppHandle,
    task: String,
    rows: u16,
    cols: u16,
    on_msg: Channel<InvokeResponseBody>,
) -> Result<Opened, BridgeError> {
    joined(
        tauri::async_runtime::spawn_blocking(move || {
            let c = app.state::<Cockpit>();
            let out = on_msg.clone();
            c.attachments.open(
                &c.socket,
                &task,
                rows,
                cols,
                move |bytes| {
                    let _ = out.send(output_msg(bytes));
                },
                move || {
                    let _ = on_msg.send(end_msg());
                },
            )
        })
        .await,
    )
}

/// Typed bytes arrive as base64 (a JSON array of numbers is about four times larger).
fn typed_bytes(b64: &str) -> Result<Vec<u8>, BridgeError> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| BridgeError::Protocol { message: format!("typed input is not base64: {e}") })
}

/// Off the main thread: a daemon that stops reading must not freeze the window. The bridge
/// bounds the write (`attach::WRITE_TIMEOUT`) and reports `BridgeError::Timeout`. The
/// frontend keeps one write in flight per attachment, so keys cannot overtake each other here.
#[tauri::command]
async fn attach_write(app: AppHandle, id: u32, b64: String) -> Result<(), BridgeError> {
    let data = typed_bytes(&b64)?;
    joined(tauri::async_runtime::spawn_blocking(move || app.state::<Cockpit>().attachments.write(id, &data)).await)
}

/// The frontend drew `bytes` more of an attachment's output (its flow-control window; see
/// `mirante_bridge::attach`). Never blocks.
#[tauri::command]
fn attach_ack(app: State<'_, Cockpit>, id: u32, bytes: usize) {
    app.attachments.ack(id, bytes);
}

#[tauri::command]
async fn attach_close(app: AppHandle, id: u32) -> Result<(), BridgeError> {
    joined(
        tauri::async_runtime::spawn_blocking(move || {
            app.state::<Cockpit>().attachments.close(id);
            Ok(())
        })
        .await,
    )
}

#[tauri::command]
async fn daemon_start(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || mirante_bridge::daemon::ensure_running(&app.state::<Cockpit>().paths))
        .await
        .map_err(|e| e.to_string())?
}

/// What the `mirante-daemon` the app would start says it is (`--version`: no shell, a time
/// limit); null when it cannot tell. The frontend compares its build with `daemon.status`'s.
#[tauri::command]
async fn daemon_binary_build() -> Option<DaemonBinary> {
    tauri::async_runtime::spawn_blocking(mirante_bridge::daemon::binary_version).await.ok().flatten()
}

/// Restarts the daemon (after an update): `daemon.shutdown`, then the same start as
/// `daemon_start`. Live sessions end and come back on their conversations when auto-resume
/// is on. The webview cannot call `daemon.shutdown` itself (the bridge's allowlist).
#[tauri::command]
async fn daemon_restart(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || mirante_bridge::daemon::restart(&app.state::<Cockpit>().paths))
        .await
        .map_err(|e| e.to_string())?
}

/// The user's home directory, for "~/…" typed in Add project.
#[tauri::command]
fn home_dir(app: AppHandle) -> Result<String, String> {
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    home.to_str().map(str::to_owned).ok_or_else(|| "the home directory is not valid UTF-8".to_string())
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PathKind {
    Dir,
    File,
    Missing,
}

/// What is at `path` (following symlinks); relative paths are refused.
fn path_kind_of(path: &std::path::Path) -> Result<PathKind, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    Ok(match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => PathKind::Dir,
        Ok(_) => PathKind::File,
        Err(_) => PathKind::Missing,
    })
}

/// Whether a project path typed in Add project is a folder (a friendly check before adding).
#[tauri::command]
fn path_kind(path: String) -> Result<PathKind, String> {
    path_kind_of(std::path::Path::new(&path))
}

/// The git checkouts in a folder given to Add project (see `scan`): read-only, bounded, off
/// the main thread.
#[tauri::command]
async fn scan_repos(path: String) -> Result<scan::Scan, String> {
    tauri::async_runtime::spawn_blocking(move || scan::scan(&path, scan::LIMITS)).await.map_err(|e| e.to_string())?
}

pub fn run() {
    let paths = Paths::from_env().expect("cannot locate ~/.mirante (is HOME set?)");
    let socket = paths.socket();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("nav-guard")
                .on_navigation(|_, url| is_app_url(url, cfg!(debug_assertions)))
                .build(),
        )
        .setup(|app| {
            // `create: false` in tauri.conf.json: the main window is built here so it can
            // refuse every new-window request (Tauri would otherwise let the platform decide).
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .cloned()
                .ok_or("tauri.conf.json has no \"main\" window")?;
            // `visible: false` in tauri.conf.json: the window shows once its page has loaded
            // (the launch splash in index.html is painted by then), never as a white rectangle;
            // at the latest after SHOW_AT_THE_LATEST, whatever happened to the page.
            let shown = Arc::new(AtomicBool::new(false));
            let on_load = shown.clone();
            let window = WebviewWindowBuilder::from_config(app.handle(), &config)?
                .on_new_window(|_, _| NewWindowResponse::Deny)
                .on_page_load(move |w, p| {
                    if p.event() == PageLoadEvent::Finished && !on_load.swap(true, Ordering::SeqCst) {
                        let _ = w.show();
                    }
                })
                .build()?;
            if let Ok(theme) = window.theme() {
                let _ = window.set_background_color(Some(backdrop(theme)));
            }
            std::thread::spawn(move || {
                std::thread::sleep(SHOW_AT_THE_LATEST);
                if !shown.swap(true, Ordering::SeqCst) {
                    let _ = window.show();
                }
            });
            Ok(())
        })
        .manage(Cockpit { paths, socket, stream: Mutex::new(None), attachments: Attachments::default() })
        .invoke_handler(tauri::generate_handler![
            rpc,
            subscribe,
            attach_open,
            attach_write,
            attach_ack,
            attach_close,
            daemon_start,
            daemon_binary_build,
            daemon_restart,
            home_dir,
            path_kind,
            scan_repos
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                window.state::<Cockpit>().attachments.close_all();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the Mirante cockpit");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> tauri::Url {
        s.parse().unwrap()
    }

    #[test]
    fn the_app_origin_is_the_only_place_the_window_may_go() {
        for ok in ["tauri://localhost/", "tauri://localhost/index.html#/task/t1", "http://tauri.localhost/", "https://tauri.localhost/x"] {
            assert!(is_app_url(&url(ok), false), "{ok}");
        }
        for bad in [
            "https://github.com/acme/shop/pull/1",
            "https://evil.example/",
            "http://localhost:1420/",
            "tauri://evil/",
            "http://tauri.localhost.evil.example/",
            "file:///etc/passwd",
            "about:blank",
            "javascript:alert(1)",
        ] {
            assert!(!is_app_url(&url(bad), false), "{bad}");
        }
    }

    #[test]
    fn typed_input_is_decoded_from_base64_and_garbage_is_a_protocol_error() {
        assert_eq!(typed_bytes("bHMN/w==").unwrap(), vec![0x6c, 0x73, 0x0d, 0xff]);
        assert!(matches!(typed_bytes("not base64!"), Err(BridgeError::Protocol { .. })));
    }

    #[test]
    fn path_kind_tells_folders_files_and_missing_paths_apart() {
        let dir = std::env::temp_dir().join(format!("mirante-cockpit-pk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(path_kind_of(&dir), Ok(PathKind::Dir));
        assert_eq!(path_kind_of(&file), Ok(PathKind::File));
        assert_eq!(path_kind_of(&dir.join("nope")), Ok(PathKind::Missing));
        assert!(path_kind_of(std::path::Path::new("relative/x")).is_err());
        assert_eq!(serde_json::to_value(PathKind::Missing).unwrap(), "missing");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_goes_out_raw_and_the_end_as_json() {
        assert!(matches!(output_msg(vec![0, 0xff, b'a']), InvokeResponseBody::Raw(b) if b == vec![0, 0xff, b'a']));
        match end_msg() {
            InvokeResponseBody::Json(j) => {
                assert_eq!(serde_json::from_str::<Value>(&j).unwrap(), serde_json::json!({"type": "end"}))
            }
            InvokeResponseBody::Raw(_) => panic!("the end is JSON"),
        }
    }

    #[test]
    fn the_backdrop_is_the_app_background_of_each_theme() {
        let page = include_str!("../../index.html");
        for (theme, hex) in [(Theme::Dark, "#1b1c21"), (Theme::Light, "#f8f7f4")] {
            let Color(r, g, b, a) = backdrop(theme);
            assert_eq!(format!("#{r:02x}{g:02x}{b:02x}"), hex);
            assert_eq!(a, 0xff);
            assert!(page.contains(&format!("--splash-bg: {hex}")), "{hex} is the splash's background too");
        }
    }

    #[test]
    fn the_dev_server_is_allowed_only_in_dev() {
        assert!(is_app_url(&url("http://127.0.0.1:1420/src/main.tsx"), true));
        assert!(!is_app_url(&url("http://127.0.0.1:1420/"), false));
        assert!(!is_app_url(&url("http://127.0.0.1:1421/"), true));
    }
}
