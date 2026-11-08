//! Tauri shell of the cockpit: six commands that forward to `mirante-bridge`. No state of
//! its own beyond the running stream and open attachments; the daemon owns everything.
//! The window never leaves the app's origin: links open in the browser through the opener
//! plugin (github.com only, see `capabilities/default.json`), never in the window itself.
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;
use mirante_bridge::attach::{Attachments, Opened};
use mirante_bridge::stream::{self, Stream, StreamMsg};
use mirante_bridge::{rpc as bridge_rpc, BridgeError};
use mirante_ipc::Paths;
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, Url, WindowEvent};

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

struct Cockpit {
    paths: Paths,
    socket: PathBuf,
    stream: Mutex<Option<Stream>>,
    attachments: Attachments,
}

/// Terminal output for the frontend: base64 keeps binary-safe bytes cheap over JSON IPC.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AttachMsg {
    Data { b64: String },
    End,
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
    on_msg: Channel<AttachMsg>,
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
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let _ = out.send(AttachMsg::Data { b64 });
                },
                move || {
                    let _ = on_msg.send(AttachMsg::End);
                },
            )
        })
        .await,
    )
}

/// Off the main thread: a daemon that stops reading must not freeze the window. The bridge
/// bounds the write (`attach::WRITE_TIMEOUT`) and reports `BridgeError::Timeout`.
#[tauri::command]
async fn attach_write(app: AppHandle, id: u32, data: Vec<u8>) -> Result<(), BridgeError> {
    joined(tauri::async_runtime::spawn_blocking(move || app.state::<Cockpit>().attachments.write(id, &data)).await)
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

pub fn run() {
    let paths = Paths::from_env().expect("cannot locate ~/.mirante (is HOME set?)");
    let socket = paths.socket();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("nav-guard")
                .on_navigation(|_, url| is_app_url(url, cfg!(debug_assertions)))
                .build(),
        )
        .manage(Cockpit { paths, socket, stream: Mutex::new(None), attachments: Attachments::default() })
        .invoke_handler(tauri::generate_handler![rpc, subscribe, attach_open, attach_write, attach_close, daemon_start])
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
    fn the_dev_server_is_allowed_only_in_dev() {
        assert!(is_app_url(&url("http://127.0.0.1:1420/src/main.tsx"), true));
        assert!(!is_app_url(&url("http://127.0.0.1:1420/"), false));
        assert!(!is_app_url(&url("http://127.0.0.1:1421/"), true));
    }
}
