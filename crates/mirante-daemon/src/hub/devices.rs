//! `device.*`: the iOS Simulator beside a task (the cockpit's side pane Device), streamed
//! by serve-sim (Apache-2.0, run as a separate program, never bundled). A person only (see
//! [`crate::access`]); agents get nothing.
//!
//! - **simctl** runs through [`run_capped`]: `xcrun` by absolute path, no shell, a cleared
//!   environment, a timeout and a capped output. A udid must have the UUID shape and be in
//!   `simctl list devices available`.
//! - **serve-sim** is the pinned [`SERVE_SIM_VERSION`], from Mirante's tools folder
//!   (`<home>/tools`, installed only by `device.tool_install` on a click) or a global
//!   install of that same version. It runs as `node <cli> --no-preview -q -p <port>
//!   <udid>`, never `--detach`, never `--allow-exec`; its auth token (32 random bytes) goes
//!   in `SERVE_SIM_AUTH_TOKEN`, not on the command line; the environment is cleared to
//!   `HOME`, a minimal `PATH`, `TMPDIR=<home>/sim/tmp` (0700) and `SERVE_SIM_CONTROL=0`. It
//!   must answer on 127.0.0.1 and not on the machine's LAN addresses, else it is killed.
//! - **Lifecycle**: one serve-sim per device, shared by the streams that view it; the last
//!   stream's stop leaves it running [`STREAM_GRACE`] more (a tab switched back finds it),
//!   then it is killed with everything it started. Archiving a task lets go of its streams;
//!   daemon shutdown kills every serve-sim. A device is never shut down without the
//!   person's click (`device.shutdown`).
//! - serve-sim trusts loopback callers without the token (its `request-trust.ts`): another
//!   local process can reach the port. Agents can already run `xcrun simctl` themselves, so
//!   this adds no privilege; the token matters if the port were ever reachable off-box.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mirante_core::model::Lifecycle;
use mirante_core::safedir::SafeDir;
use mirante_ipc::types::{
    DeviceSetup, DeviceStreamGrant, DeviceStreamStart, DeviceView, ServeSimFound, ToolFound, ToolView,
};
use serde::Deserialize;
use serde_json::Value;

use super::{Hub, HubError, Result};
use crate::driver::proctree::{self, Proc};

/// The serve-sim release Mirante runs (read and pinned in the side-pane plan's spike S1).
pub const SERVE_SIM_VERSION: &str = "0.1.49";
/// Its npm tarball's integrity, as the registry publishes it.
pub const SERVE_SIM_INTEGRITY: &str =
    "sha512-Q2SC29YDY30+qrfNA7Auv4etGqe++FYT3bionmpnKi3kd1xgisI/ohSyrrLqD9N5UsBMXjITcVecxMCQAV5sgg==";
/// How long a serve-sim nobody views keeps running.
pub const STREAM_GRACE: Duration = Duration::from_secs(60);
/// The oldest Node serve-sim runs on.
pub const NODE_MAJOR: u32 = 20;
/// A simctl list or a version check.
const QUICK: Duration = Duration::from_secs(20);
/// `simctl boot` / `shutdown` (a cold boot takes a while).
const BOOT: Duration = Duration::from_secs(180);
/// `npm install`.
const INSTALL: Duration = Duration::from_secs(300);
/// serve-sim's start line.
const START: Duration = Duration::from_secs(30);
/// Most output read from a helper program.
const MAX_OUT: usize = 8 * 1024 * 1024;
/// SIGTERM → SIGKILL grace when serve-sim is stopped.
const KILL_GRACE: Duration = Duration::from_millis(500);
/// The minimal `PATH` beside node's own folder.
const BASE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The programs the device surface runs. [`DeviceTools::detect`] finds them; tests set fakes
/// ([`Hub::set_device_tools`]).
#[derive(Debug, Clone)]
pub struct DeviceTools {
    pub xcrun: PathBuf,
    /// `node`, when found.
    pub node: Option<PathBuf>,
    /// `npm`, when found (beside node).
    pub npm: Option<PathBuf>,
    pub arm64: bool,
    /// Global `node_modules` folders where a serve-sim of the pinned version may be.
    pub globals: Vec<PathBuf>,
}

impl DeviceTools {
    /// `/usr/bin/xcrun`, and `node` / `npm` from the daemon's `PATH` or the usual install
    /// places (Homebrew, `/usr/local`).
    pub fn detect() -> DeviceTools {
        let mut dirs: Vec<PathBuf> =
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
            dirs.push(PathBuf::from(d));
        }
        let find = |name: &str| dirs.iter().filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file());
        let node = find("node");
        let npm = node.as_ref().and_then(|n| n.parent()).map(|d| d.join("npm")).filter(|p| p.is_file());
        let globals =
            node.as_ref().and_then(|n| n.parent()?.parent().map(|p| p.join("lib/node_modules"))).into_iter().collect();
        DeviceTools {
            xcrun: PathBuf::from("/usr/bin/xcrun"),
            node,
            npm,
            arm64: std::env::consts::ARCH == "aarch64",
            globals,
        }
    }

    fn node_dir(&self) -> Option<PathBuf> {
        self.node.as_ref().and_then(|n| n.parent()).map(Path::to_path_buf)
    }

    /// `PATH` for what the device surface runs: node's folder, then the system's.
    fn path(&self) -> String {
        match self.node_dir() {
            Some(d) => format!("{}:{BASE_PATH}", d.display()),
            None => BASE_PATH.to_string(),
        }
    }
}

/// A running serve-sim.
struct Server {
    child: Child,
    root: Option<Proc>,
    port: u16,
    token: String,
    /// When its last stream stopped (`None` while viewed).
    idle_since: Option<Instant>,
    /// Bumped each time it goes idle: a grace timer acts only on its own generation.
    gen: u64,
}

impl Server {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill(mut self) {
        if let Some(root) = self.root {
            proctree::kill_tree_with(root, &[], false, KILL_GRACE, &proctree::Kill);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct StreamRef {
    udid: String,
    task: Option<String>,
}

/// serve-sims by device, and the streams that view them.
pub(crate) struct Registry {
    servers: HashMap<String, Server>,
    streams: HashMap<String, StreamRef>,
    /// Tests: fake programs (else [`DeviceTools::detect`] per call).
    pub(crate) tools: Option<DeviceTools>,
    /// How long an unviewed serve-sim keeps running ([`STREAM_GRACE`]; tests shorten it).
    pub(crate) grace: Duration,
    /// A `device.tool_install` runs (one at a time).
    pub(crate) installing: bool,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            servers: HashMap::new(),
            streams: HashMap::new(),
            tools: None,
            grace: STREAM_GRACE,
            installing: false,
        }
    }
}

impl Registry {
    fn prune(&mut self) {
        let dead: Vec<String> = self.servers.iter_mut().filter_map(|(u, s)| (!s.alive()).then(|| u.clone())).collect();
        for u in dead {
            if let Some(s) = self.servers.remove(&u) {
                s.kill();
            }
            self.streams.retain(|_, r| r.udid != u);
        }
    }

    /// The device's serve-sim goes idle when no stream views it; a timer kills it after
    /// the grace period unless a stream came back meanwhile.
    fn release(&mut self, udid: &str, reg: &Arc<Mutex<Registry>>) {
        if self.streams.values().any(|r| r.udid == udid) {
            return;
        }
        let grace = self.grace;
        let Some(s) = self.servers.get_mut(udid) else { return };
        s.gen += 1;
        s.idle_since = Some(Instant::now());
        let gen = s.gen;
        let (reg, udid) = (Arc::clone(reg), udid.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(grace);
            let gone = {
                let mut r = lock(&reg);
                let idle = r.servers.get(&udid).is_some_and(|s| s.gen == gen && s.idle_since.is_some());
                if idle {
                    r.servers.remove(&udid)
                } else {
                    None
                }
            };
            if let Some(s) = gone {
                s.kill();
            }
        });
    }
}

fn lock(r: &Mutex<Registry>) -> MutexGuard<'_, Registry> {
    r.lock().unwrap_or_else(|e| e.into_inner())
}

/// A program's stdout, or why it failed: run without a shell, with exactly `env`, killed
/// after `limit`, its output capped at [`MAX_OUT`].
pub fn run_capped(program: &Path, args: &[&str], env: &[(&str, String)], limit: Duration) -> std::io::Result<Vec<u8>> {
    let mut cmd = Command::new(program);
    cmd.args(args).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn()?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(r) = r {
                let _ = r.take(MAX_OUT as u64 + 1).read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            // SAFETY: plain syscall on the group the child leads (`process_group(0)`).
            unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{} took longer than {limit:?}", program.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if stdout.len() > MAX_OUT {
        return Err(std::io::Error::other(format!("{} printed more than {MAX_OUT} bytes", program.display())));
    }
    if !status.success() {
        let tail = String::from_utf8_lossy(&stderr[stderr.len().saturating_sub(2000)..]).trim().to_string();
        return Err(std::io::Error::other(format!("{} failed ({status}): {tail}", program.display())));
    }
    Ok(stdout)
}

/// A simulator udid: `8-4-4-4-12` hex digits, upper case as simctl prints them.
pub fn udid_shaped(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)))
}

#[derive(Deserialize)]
struct SimDevice {
    udid: String,
    name: String,
    state: String,
    #[serde(default = "yes", rename = "isAvailable")]
    is_available: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct SimRuntime {
    identifier: String,
    name: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default = "yes", rename = "isAvailable")]
    is_available: bool,
}

fn is_ios(identifier: &str, platform: Option<&str>) -> bool {
    match platform {
        Some(p) => p == "iOS",
        None => identifier.rsplit('.').next().is_some_and(|r| r.starts_with("iOS-")),
    }
}

/// `simctl list runtimes --json`: the available iOS runtimes, identifier → name.
pub fn parse_runtimes(json: &[u8]) -> std::result::Result<HashMap<String, String>, String> {
    #[derive(Deserialize)]
    struct L {
        runtimes: Vec<SimRuntime>,
    }
    let l: L = serde_json::from_slice(json).map_err(|e| format!("simctl runtimes: {e}"))?;
    Ok(l.runtimes
        .into_iter()
        .filter(|r| r.is_available && is_ios(&r.identifier, r.platform.as_deref()))
        .map(|r| (r.identifier, r.name))
        .collect())
}

/// `simctl list devices available --json`: the iOS simulators, by runtime name (newest
/// first), then name. A runtime missing from `runtimes` is named from its identifier.
pub fn parse_devices(json: &[u8], runtimes: &HashMap<String, String>) -> std::result::Result<Vec<DeviceView>, String> {
    #[derive(Deserialize)]
    struct L {
        devices: HashMap<String, Vec<SimDevice>>,
    }
    let l: L = serde_json::from_slice(json).map_err(|e| format!("simctl devices: {e}"))?;
    let mut out = Vec::new();
    for (rt, devs) in l.devices {
        if !is_ios(&rt, None) && !runtimes.contains_key(&rt) {
            continue;
        }
        let runtime = runtimes
            .get(&rt)
            .cloned()
            .unwrap_or_else(|| rt.rsplit('.').next().unwrap_or(&rt).replacen('-', " ", 1).replace('-', "."));
        for d in devs {
            if d.is_available && udid_shaped(&d.udid) {
                out.push(DeviceView {
                    udid: d.udid,
                    name: d.name,
                    runtime: runtime.clone(),
                    state: d.state,
                    streaming: false,
                });
            }
        }
    }
    let key = |r: &str| -> Vec<u32> { r.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect() };
    out.sort_by(|a, b| key(&b.runtime).cmp(&key(&a.runtime)).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// `node --version` (`v22.11.0`) → its major version.
pub fn node_major(version: &str) -> Option<u32> {
    version.trim().strip_prefix('v')?.split('.').next()?.parse().ok()
}

/// A `package.json`'s `version`.
fn package_version(dir: &Path) -> Option<String> {
    let f = std::fs::File::open(dir.join("package.json")).ok()?;
    let mut buf = Vec::new();
    f.take(256 * 1024).read_to_end(&mut buf).ok()?;
    let v: Value = serde_json::from_slice(&buf).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

/// serve-sim's entry point in a package folder.
fn serve_sim_cli(pkg: &Path) -> PathBuf {
    pkg.join("dist/serve-sim.js")
}

/// serve-sim's arguments (after the cli path): stream only, JSON output, the starting
/// port, the device. Never `--detach` (`-d`), never `--allow-exec`; the token is not here
/// (see [`serve_sim_env`]).
pub fn serve_sim_args(cli: &Path, port: u16, udid: &str) -> Vec<String> {
    vec![
        cli.to_string_lossy().into_owned(),
        "--no-preview".into(),
        "-q".into(),
        "-p".into(),
        port.to_string(),
        udid.to_string(),
    ]
}

/// serve-sim's whole environment: `HOME`, `PATH` (node's folder, then the system's),
/// `TMPDIR`, its background control process off, and the auth token.
pub fn serve_sim_env(home: &Path, path: &str, tmpdir: &Path, token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("HOME", home.to_string_lossy().into_owned()),
        ("PATH", path.to_string()),
        ("TMPDIR", format!("{}/", tmpdir.display())),
        ("SERVE_SIM_CONTROL", "0".into()),
        ("SERVE_SIM_AUTH_TOKEN", token.to_string()),
    ]
}

/// `npm`'s arguments for `device.tool_install`: the pinned version into `prefix`, no
/// install scripts, no audit or funding calls.
pub fn install_args(prefix: &Path) -> Vec<String> {
    vec![
        "install".into(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
        "--ignore-scripts".into(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--save-exact".into(),
        format!("serve-sim@{SERVE_SIM_VERSION}"),
    ]
}

/// 32 random bytes, hex.
pub fn random_token() -> std::io::Result<String> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A port free on 127.0.0.1 now (serve-sim may still move up from it; its start line says).
fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The machine's non-loopback addresses (IPv4 and global IPv6).
fn lan_addrs() -> Vec<IpAddr> {
    let mut out = Vec::new();
    let mut ifa: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `ifa` with a list freed by freeifaddrs below; each node's
    // `ifa_addr` is null or a sockaddr of the family it names.
    unsafe {
        if libc::getifaddrs(&mut ifa) != 0 {
            return out;
        }
        let mut p = ifa;
        while !p.is_null() {
            let a = (*p).ifa_addr;
            if !a.is_null() {
                match i32::from((*a).sa_family) {
                    libc::AF_INET => {
                        let sin = &*(a as *const libc::sockaddr_in);
                        let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        if !ip.is_loopback() && !ip.is_unspecified() {
                            out.push(IpAddr::V4(ip));
                        }
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(a as *const libc::sockaddr_in6);
                        let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                        // Link-local needs a scope id to connect; global ones are enough.
                        if !ip.is_loopback() && !ip.is_unspecified() && (ip.segments()[0] & 0xffc0) != 0xfe80 {
                            out.push(IpAddr::V6(ip));
                        }
                    }
                    _ => {}
                }
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(ifa);
    }
    out
}

/// Ok when `port` answers on 127.0.0.1 and on none of `lan`.
pub fn loopback_only(port: u16, lan: &[IpAddr]) -> std::result::Result<(), String> {
    let t = Duration::from_millis(500);
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), t)
        .map_err(|e| format!("serve-sim does not answer on 127.0.0.1:{port}: {e}"))?;
    for ip in lan {
        if TcpStream::connect_timeout(&SocketAddr::new(*ip, port), Duration::from_millis(300)).is_ok() {
            return Err(format!("serve-sim answers on {ip}:{port}, not only on loopback; stopped"));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct StartLine {
    port: u16,
    #[serde(default)]
    url: Option<String>,
}

/// serve-sim's JSON start line: its port, if its URL (when given) is a loopback one.
pub fn parse_start_line(line: &str) -> Option<std::result::Result<u16, String>> {
    let s: StartLine = serde_json::from_str(line.trim()).ok()?;
    if let Some(url) = &s.url {
        let host = url.strip_prefix("http://").and_then(|r| r.split([':', '/']).next()).unwrap_or("");
        if !matches!(host, "127.0.0.1" | "localhost") {
            return Some(Err(format!("serve-sim listens on {url}, not on loopback")));
        }
    }
    Some(Ok(s.port))
}

impl Hub {
    fn devices(&self) -> MutexGuard<'_, Registry> {
        lock(&self.devices)
    }

    /// Tests: fake `xcrun` / `node` / `npm`.
    pub fn set_device_tools(&self, tools: DeviceTools) {
        self.devices().tools = Some(tools);
    }

    /// Tests: how long an unviewed serve-sim keeps running.
    pub fn set_device_grace(&self, grace: Duration) {
        self.devices().grace = grace;
    }

    fn device_tools(&self) -> DeviceTools {
        self.devices().tools.clone().unwrap_or_else(DeviceTools::detect)
    }

    fn tools_dir(&self) -> PathBuf {
        self.paths.home().join("tools")
    }

    /// `HOME` for the programs the device surface runs.
    fn person_home(&self) -> PathBuf {
        super::home_dir().unwrap_or_else(|| self.paths.home().to_path_buf())
    }

    fn xcrun(&self, tools: &DeviceTools, args: &[&str], limit: Duration) -> Result<Vec<u8>> {
        let env = [("HOME", self.person_home().to_string_lossy().into_owned()), ("PATH", BASE_PATH.to_string())];
        run_capped(&tools.xcrun, args, &env, limit).map_err(|e| HubError::Conflict(e.to_string()))
    }

    /// Where the pinned serve-sim is: Mirante's tools folder first, then a global install.
    /// The second value is another version found (not used).
    fn find_serve_sim(&self, tools: &DeviceTools) -> (Option<PathBuf>, Option<String>) {
        let mut other = None;
        let local = self.tools_dir().join("node_modules/serve-sim");
        for pkg in std::iter::once(local).chain(tools.globals.iter().map(|g| g.join("serve-sim"))) {
            match package_version(&pkg) {
                Some(v) if v == SERVE_SIM_VERSION && serve_sim_cli(&pkg).is_file() => return (Some(pkg), None),
                Some(v) => other = other.or(Some(v)),
                None => {}
            }
        }
        (None, other)
    }

    fn install_command(&self, tools: &DeviceTools) -> String {
        let npm = tools.npm.as_ref().map_or_else(|| "npm".to_string(), |p| p.to_string_lossy().into_owned());
        std::iter::once(npm).chain(install_args(&self.tools_dir())).collect::<Vec<_>>().join(" ")
    }

    /// `device.setup`.
    pub fn device_setup(&self) -> Result<DeviceSetup> {
        let tools = self.device_tools();
        let mut s = DeviceSetup {
            arm64: tools.arm64,
            install_command: self.install_command(&tools),
            serve_sim: ServeSimFound { pinned: SERVE_SIM_VERSION.into(), ..Default::default() },
            ..Default::default()
        };
        if !tools.arm64 {
            // serve-sim runs on Apple Silicon only: nothing else to check.
            return Ok(s);
        }
        s.xcode = self.xcrun(&tools, &["simctl", "help"], QUICK).is_ok();
        if s.xcode {
            s.ios_runtime = self
                .xcrun(&tools, &["simctl", "list", "runtimes", "--json"], QUICK)
                .ok()
                .and_then(|out| parse_runtimes(&out).ok())
                .is_some_and(|r| !r.is_empty());
        }
        if let Some(node) = &tools.node {
            let env = [("HOME", self.person_home().to_string_lossy().into_owned()), ("PATH", tools.path())];
            let v = run_capped(node, &["--version"], &env, QUICK)
                .ok()
                .map(|o| String::from_utf8_lossy(&o).trim().to_string());
            s.node =
                ToolFound { found: v.as_deref().and_then(node_major).is_some_and(|m| m >= NODE_MAJOR), version: v };
        }
        let (pkg, other) = self.find_serve_sim(&tools);
        s.serve_sim.installed = pkg.is_some();
        s.serve_sim.version = pkg.as_ref().map(|_| SERVE_SIM_VERSION.to_string());
        s.serve_sim.other = other;
        s.ready = s.arm64 && s.xcode && s.ios_runtime && s.node.found && s.serve_sim.installed;
        Ok(s)
    }

    /// `device.list`: the available iOS simulators, marked when a serve-sim streams them.
    pub fn device_list(&self) -> Result<Vec<DeviceView>> {
        let tools = self.device_tools();
        let rt = self.xcrun(&tools, &["simctl", "list", "runtimes", "--json"], QUICK)?;
        let rt = parse_runtimes(&rt).map_err(HubError::Conflict)?;
        let out = self.xcrun(&tools, &["simctl", "list", "devices", "available", "--json"], QUICK)?;
        let mut list = parse_devices(&out, &rt).map_err(HubError::Conflict)?;
        let mut reg = self.devices();
        reg.prune();
        for d in &mut list {
            d.streaming = reg.servers.contains_key(&d.udid);
        }
        Ok(list)
    }

    /// The listed device `udid` (refused when it is not udid-shaped or not listed).
    fn listed(&self, udid: &str) -> Result<DeviceView> {
        if !udid_shaped(udid) {
            return Err(HubError::Invalid(format!("{udid:?} is not a simulator udid")));
        }
        self.device_list()?
            .into_iter()
            .find(|d| d.udid == udid)
            .ok_or_else(|| HubError::NotFound(format!("no available iOS simulator {udid}")))
    }

    /// `device.boot`.
    pub fn device_boot(&self, udid: &str) -> Result<()> {
        let d = self.listed(udid)?;
        if d.state == "Booted" {
            return Ok(());
        }
        self.xcrun(&self.device_tools(), &["simctl", "boot", &d.udid], BOOT).map(|_| ())
    }

    /// `device.shutdown` (the cockpit asked the person first): its serve-sim stops too.
    pub fn device_shutdown(&self, udid: &str) -> Result<()> {
        let d = self.listed(udid)?;
        let gone = {
            let mut reg = self.devices();
            reg.streams.retain(|_, r| r.udid != d.udid);
            reg.servers.remove(&d.udid)
        };
        if let Some(s) = gone {
            s.kill();
        }
        if d.state == "Shutdown" {
            return Ok(());
        }
        self.xcrun(&self.device_tools(), &["simctl", "shutdown", &d.udid], BOOT).map(|_| ())
    }

    /// `device.tool_install`: the command `device.setup` showed, then the installed
    /// version and the lockfile's integrity checked against the pinned ones (a mismatch
    /// removes what was installed).
    pub fn device_tool_install(&self) -> Result<DeviceSetup> {
        let tools = self.device_tools();
        let npm = tools.npm.clone().ok_or_else(|| HubError::Conflict("npm was not found (install Node 20+)".into()))?;
        {
            let mut reg = self.devices();
            if reg.installing {
                return Err(HubError::Conflict("serve-sim is being installed".into()));
            }
            reg.installing = true;
        }
        let r = self.install_serve_sim(&tools, &npm);
        self.devices().installing = false;
        r?;
        self.device_setup()
    }

    fn install_serve_sim(&self, tools: &DeviceTools, npm: &Path) -> Result<()> {
        let dir =
            SafeDir::create(self.paths.home(), &["tools"], 0o700).map_err(|e| HubError::Conflict(e.to_string()))?;
        let prefix = dir.path().to_path_buf();
        let args = install_args(&prefix);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let env = [
            ("HOME", self.person_home().to_string_lossy().into_owned()),
            ("PATH", tools.path()),
            ("npm_config_ignore_scripts", "true".to_string()),
            ("npm_config_update_notifier", "false".to_string()),
        ];
        run_capped(npm, &args, &env, INSTALL).map_err(|e| HubError::Conflict(format!("npm install failed: {e}")))?;
        let pkg = prefix.join("node_modules/serve-sim");
        let refuse = |why: String| {
            let _ = std::fs::remove_dir_all(&pkg);
            HubError::Conflict(why)
        };
        match package_version(&pkg) {
            Some(v) if v == SERVE_SIM_VERSION => {}
            v => return Err(refuse(format!("npm installed serve-sim {v:?}, not {SERVE_SIM_VERSION}"))),
        }
        let lock = std::fs::File::open(prefix.join("package-lock.json")).and_then(|f| {
            let mut b = Vec::new();
            f.take(MAX_OUT as u64).read_to_end(&mut b)?;
            Ok(b)
        });
        let integrity = lock
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.pointer("/packages/node_modules~1serve-sim/integrity")?.as_str().map(str::to_string));
        if integrity.as_deref() != Some(SERVE_SIM_INTEGRITY) {
            return Err(refuse(format!("serve-sim's integrity {integrity:?} is not the pinned one")));
        }
        if !serve_sim_cli(&pkg).is_file() {
            return Err(refuse("serve-sim's dist/serve-sim.js is missing".into()));
        }
        Ok(())
    }

    /// `device.stream_start`: shares the device's serve-sim, or starts one (the device must
    /// be booted, serve-sim installed).
    pub fn device_stream_start(&self, p: &DeviceStreamStart) -> Result<DeviceStreamGrant> {
        if self.is_shutting_down() {
            return Err(HubError::Conflict("the daemon is shutting down".into()));
        }
        let task = match &p.task {
            Some(t) => {
                let st = self.lock();
                let t = st.resolve(t)?;
                if matches!(t.lifecycle, Lifecycle::Archived | Lifecycle::Archiving) {
                    return Err(HubError::Conflict(format!("task {} is archived", t.branch)));
                }
                Some(t.id)
            }
            None => None,
        };
        let d = self.listed(&p.udid)?;
        if d.state != "Booted" {
            return Err(HubError::Conflict(format!("{} is not booted (boot it first)", d.name)));
        }
        let tools = self.device_tools();
        let (pkg, _) = self.find_serve_sim(&tools);
        let pkg = pkg.ok_or_else(|| HubError::Conflict(format!("serve-sim {SERVE_SIM_VERSION} is not installed")))?;
        let node = tools.node.clone().ok_or_else(|| HubError::Conflict("node was not found".into()))?;
        let mut reg = self.devices();
        reg.prune();
        if !reg.servers.contains_key(&d.udid) {
            let s = self.spawn_serve_sim(&tools, &node, &serve_sim_cli(&pkg), &d.udid)?;
            reg.servers.insert(d.udid.clone(), s);
        }
        let s = reg.servers.get_mut(&d.udid).expect("just inserted");
        s.idle_since = None;
        s.gen += 1;
        let grant = DeviceStreamGrant {
            stream: mirante_core::model::new_id(),
            udid: d.udid.clone(),
            port: s.port,
            token: s.token.clone(),
        };
        reg.streams.insert(grant.stream.clone(), StreamRef { udid: d.udid, task });
        drop(reg);
        // A shutdown that began meanwhile killed what it found; this one too.
        if self.is_shutting_down() {
            self.close_all_devices();
            return Err(HubError::Conflict("the daemon is shutting down".into()));
        }
        Ok(grant)
    }

    fn spawn_serve_sim(&self, tools: &DeviceTools, node: &Path, cli: &Path, udid: &str) -> Result<Server> {
        let fail = |e: std::io::Error| HubError::Conflict(format!("could not start serve-sim: {e}"));
        let tmp = SafeDir::create(self.paths.home(), &["sim", "tmp"], 0o700)
            .map_err(|e| HubError::Conflict(e.to_string()))?;
        let port = free_port().map_err(fail)?;
        let token = random_token().map_err(fail)?;
        let mut cmd = Command::new(node);
        cmd.args(serve_sim_args(cli, port, udid))
            .env_clear()
            .current_dir(tmp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        for (k, v) in serve_sim_env(&self.person_home(), &tools.path(), tmp.path(), &token) {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(fail)?;
        let root = proctree::proc_of(child.id() as i32);
        let stdout = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Its start line, then drained until it exits (a full pipe must not stall it).
            let mut r = BufReader::new(stdout);
            let mut line = String::new();
            let mut sent = false;
            loop {
                line.clear();
                match (&mut r).take(64 * 1024).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if !sent {
                    if let Some(port) = parse_start_line(&line) {
                        let _ = tx.send(port);
                        sent = true;
                    }
                }
            }
        });
        let mut s = Server { child, root, port, token, idle_since: None, gen: 0 };
        let started = match rx.recv_timeout(START) {
            Ok(Ok(p)) => Ok(p),
            Ok(Err(why)) => Err(why),
            Err(_) => Err(format!("serve-sim printed no start line within {START:?}")),
        };
        let checked = started.and_then(|p| loopback_only(p, &lan_addrs()).map(|()| p));
        match checked {
            Ok(p) => {
                s.port = p;
                Ok(s)
            }
            Err(why) => {
                s.kill();
                Err(HubError::Conflict(why))
            }
        }
    }

    /// `device.stream_stop`: the stream lets go of its device.
    pub fn device_stream_stop(&self, stream: &str) -> Result<()> {
        let mut reg = self.devices();
        let r = reg.streams.remove(stream).ok_or_else(|| HubError::NotFound(format!("no device stream {stream}")))?;
        reg.release(&r.udid, &self.devices);
        Ok(())
    }

    /// Lets go of the streams `task` opened (it is being archived).
    pub(crate) fn release_device_streams_of(&self, task: &str) {
        let mut reg = self.devices();
        let mine: Vec<(String, String)> = reg
            .streams
            .iter()
            .filter(|(_, r)| r.task.as_deref() == Some(task))
            .map(|(i, r)| (i.clone(), r.udid.clone()))
            .collect();
        for (id, udid) in mine {
            reg.streams.remove(&id);
            reg.release(&udid, &self.devices);
        }
    }

    /// Kills every serve-sim (daemon shutdown).
    pub(crate) fn close_all_devices(&self) {
        let gone: Vec<Server> = {
            let mut reg = self.devices();
            reg.streams.clear();
            reg.servers.drain().map(|(_, s)| s).collect()
        };
        for s in gone {
            s.kill();
        }
    }

    /// `device.tools`: the running serve-sims as the memory guard shows them.
    pub fn device_tool_views(&self) -> Vec<ToolView> {
        let mut v: Vec<ToolView> = self
            .device_servers()
            .into_iter()
            .map(|(udid, root, streams)| ToolView {
                kind: "tool".into(),
                name: "serve-sim".into(),
                udid,
                bytes: root
                    .and_then(|r| crate::memguard::sampler::sum_tree(r, crate::memguard::sampler::process_bytes))
                    .map(|t| t.bytes),
                streams,
            })
            .collect();
        v.sort_by(|a, b| a.udid.cmp(&b.udid));
        v
    }

    /// The running serve-sims (memguard reports them as tools): udid, root process, streams.
    pub(crate) fn device_servers(&self) -> Vec<(String, Option<Proc>, usize)> {
        let mut reg = self.devices();
        reg.prune();
        let reg = &*reg;
        reg.servers
            .iter()
            .map(|(u, s)| (u.clone(), s.root, reg.streams.values().filter(|r| &r.udid == u).count()))
            .collect()
    }

    /// `Stop device stream`: kills a device's serve-sim now (its viewers' streams end).
    pub fn device_stop_server(&self, udid: &str) -> Result<()> {
        let gone = {
            let mut reg = self.devices();
            reg.streams.retain(|_, r| r.udid != udid);
            reg.servers.remove(udid)
        };
        gone.map(Server::kill).ok_or_else(|| HubError::NotFound(format!("no device stream for {udid}")))
    }
}

pub(crate) type Shared = Arc<Mutex<Registry>>;

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use mirante_core::model::AgentKind;
    use std::os::unix::fs::PermissionsExt;

    const BOOTED: &str = "5B8E9D9C-0F0A-4A6B-9D0E-1C2B3A4D5E6F";
    const BOOTED_OLD: &str = "A1B2C3D4-E5F6-4789-8ABC-DEF012345678";
    const OFF: &str = "0C1D2E3F-4A5B-4C6D-8E9F-A0B1C2D3E4F5";
    const UNLISTED: &str = "99999999-8888-4777-8666-555555555555";
    const WATCH: &str = "11111111-2222-4333-8444-555555555555";

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hub/testdata").join(name)).unwrap()
    }

    fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A kit with a fake `xcrun` (the fixtures), `node`, `npm`, and (when `serve_sim`) the
    /// pinned serve-sim in Mirante's tools folder: a script that records its arguments and
    /// environment, starts a child, listens on its port and prints serve-sim's start line.
    fn kit(serve_sim: bool) -> (Kit, PathBuf) {
        let k = Kit::new();
        let fake = k.dir.path().join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::write(fake.join("devices.json"), fixture("simctl-devices.json")).unwrap();
        std::fs::write(fake.join("runtimes.json"), fixture("simctl-runtimes.json")).unwrap();
        script(
            &fake.join("xcrun"),
            r#"d=$(dirname "$0")
case "$*" in
  "simctl help") echo usage ;;
  "simctl list devices available --json") cat "$d/devices.json" ;;
  "simctl list runtimes --json") cat "$d/runtimes.json" ;;
  "simctl boot "*|"simctl shutdown "*) echo "$*" >> "$d/calls" ;;
  *) echo "unexpected: $*" >&2; exit 64 ;;
esac
"#,
        );
        script(&fake.join("node"), "if [ \"$1\" = --version ]; then echo v22.11.0; exit 0; fi\nexec /bin/sh \"$@\"\n");
        script(
            &fake.join("npm"),
            r#"d=$(dirname "$0")
echo "$@" > "$d/npm-args"
p=$3
mkdir -p "$p/node_modules/serve-sim/dist"
v=$(cat "$d/npm-version" 2>/dev/null || echo 0.1.49)
i=$(cat "$d/npm-integrity")
echo "{\"name\":\"serve-sim\",\"version\":\"$v\"}" > "$p/node_modules/serve-sim/package.json"
echo "// cli" > "$p/node_modules/serve-sim/dist/serve-sim.js"
echo "{\"packages\":{\"node_modules/serve-sim\":{\"version\":\"$v\",\"integrity\":\"$i\"}}}" > "$p/package-lock.json"
"#,
        );
        std::fs::write(fake.join("npm-integrity"), SERVE_SIM_INTEGRITY).unwrap();
        if serve_sim {
            let pkg = k.hub.paths().home().join("tools/node_modules/serve-sim");
            std::fs::create_dir_all(pkg.join("dist")).unwrap();
            std::fs::write(
                pkg.join("package.json"),
                format!(r#"{{"name":"serve-sim","version":"{SERVE_SIM_VERSION}"}}"#),
            )
            .unwrap();
            let rec = fake.display().to_string();
            script(
                &pkg.join("dist/serve-sim.js"),
                &format!(
                    r#"r={rec}
echo "$@" > "$r/args.$$"
env > "$r/env.$$"
sleep 60 &
echo $! > "$r/kid.$$"
exec /usr/bin/perl -MIO::Socket::INET -e '$s=IO::Socket::INET->new(LocalAddr=>"127.0.0.1",LocalPort=>$ARGV[0],Listen=>5,ReuseAddr=>1) or die "listen: $!"; $|=1; print qq({{"pid":$$,"port":$ARGV[0],"device":"$ARGV[1]","url":"http://127.0.0.1:$ARGV[0]"}}\n); while (my $c = $s->accept) {{ close $c }}' "$4" "$5"
"#
                ),
            );
        }
        k.hub.set_device_tools(DeviceTools {
            xcrun: fake.join("xcrun"),
            node: Some(fake.join("node")),
            npm: Some(fake.join("npm")),
            arm64: true,
            globals: Vec::new(),
        });
        (k, fake)
    }

    /// The files `prefix.*` the fake serve-sims wrote, oldest first.
    fn records(dir: &Path, prefix: &str) -> Vec<String> {
        let mut v: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{prefix}.")))
            .map(|e| (e.metadata().unwrap().modified().unwrap(), std::fs::read_to_string(e.path()).unwrap()))
            .collect();
        v.sort();
        v.into_iter().map(|(_, s)| s).collect()
    }

    fn start(k: &Kit, udid: &str) -> Result<DeviceStreamGrant> {
        k.hub.device_stream_start(&DeviceStreamStart { udid: udid.into(), task: None })
    }

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks the pid.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn simctl_json_parses_ios_devices_by_runtime() {
        let rt = parse_runtimes(&fixture("simctl-runtimes.json")).unwrap();
        assert_eq!(rt.len(), 2, "watchOS is left out: {rt:?}");
        let list = parse_devices(&fixture("simctl-devices.json"), &rt).unwrap();
        let got: Vec<(&str, &str, &str)> =
            list.iter().map(|d| (d.runtime.as_str(), d.name.as_str(), d.state.as_str())).collect();
        assert_eq!(
            got,
            [
                ("iOS 18.2", "iPad Air 11-inch (M2)", "Shutdown"),
                ("iOS 18.2", "iPhone 16", "Booted"),
                ("iOS 17.5", "iPhone 15", "Booted"),
            ]
        );
        assert!(!list.iter().any(|d| d.udid == WATCH));
        assert!(parse_runtimes(b"{\"runtimes\":[]}").unwrap().is_empty());
        assert!(parse_devices(b"not json", &rt).is_err());
    }

    #[test]
    fn udid_must_be_listed() {
        assert!(udid_shaped(BOOTED));
        for bad in
            ["", "booted", "5b8e9d9c-0f0a-4a6b-9d0e-1c2b3a4d5e6f", "5B8E9D9C-0F0A-4A6B-9D0E-1C2B3A4D5E6", "--set=/x"]
        {
            assert!(!udid_shaped(bad), "{bad}");
        }
        let (k, fake) = kit(true);
        assert!(matches!(k.hub.device_boot("--help"), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.device_boot(UNLISTED), Err(HubError::NotFound(_))));
        assert!(matches!(k.hub.device_shutdown(WATCH), Err(HubError::NotFound(_))));
        assert!(matches!(start(&k, UNLISTED), Err(HubError::NotFound(_))));
        // A listed, shut down device boots; a booted one is left alone; neither ran a shell.
        k.hub.device_boot(OFF).unwrap();
        k.hub.device_boot(BOOTED).unwrap();
        k.hub.device_shutdown(BOOTED).unwrap();
        let calls = std::fs::read_to_string(fake.join("calls")).unwrap();
        assert_eq!(calls, format!("simctl boot {OFF}\nsimctl shutdown {BOOTED}\n"));
        // Streaming needs a booted device.
        assert!(matches!(start(&k, OFF), Err(HubError::Conflict(_))));
    }

    #[test]
    fn setup_reports_missing_node_xcode_runtime() {
        let (k, fake) = kit(true);
        let s = k.hub.device_setup().unwrap();
        assert!(s.arm64 && s.xcode && s.ios_runtime && s.node.found && s.serve_sim.installed && s.ready, "{s:?}");
        assert_eq!(s.node.version.as_deref(), Some("v22.11.0"));
        assert_eq!(s.serve_sim.version.as_deref(), Some(SERVE_SIM_VERSION));
        // No iOS runtime.
        std::fs::write(fake.join("runtimes.json"), fixture("simctl-runtimes-none.json")).unwrap();
        let s = k.hub.device_setup().unwrap();
        assert!(s.xcode && !s.ios_runtime && !s.ready, "{s:?}");
        // No Xcode (xcrun fails), no node, an old node.
        let mut tools = k.hub.device_tools();
        tools.xcrun = fake.join("missing-xcrun");
        tools.node = None;
        k.hub.set_device_tools(tools.clone());
        let s = k.hub.device_setup().unwrap();
        assert!(!s.xcode && !s.ios_runtime && !s.node.found && !s.ready, "{s:?}");
        script(&fake.join("old-node"), "echo v18.19.0\n");
        tools.node = Some(fake.join("old-node"));
        k.hub.set_device_tools(tools);
        let s = k.hub.device_setup().unwrap();
        assert_eq!((s.node.found, s.node.version.as_deref()), (false, Some("v18.19.0")));
        assert_eq!(node_major("v20.0.1"), Some(20));
        assert_eq!(node_major("20"), None);
    }

    #[test]
    fn serve_sim_of_another_version_is_reported_not_used() {
        let (k, _fake) = kit(false);
        let global = k.dir.path().join("global");
        std::fs::create_dir_all(global.join("serve-sim/dist")).unwrap();
        std::fs::write(global.join("serve-sim/package.json"), r#"{"version":"0.0.1"}"#).unwrap();
        std::fs::write(global.join("serve-sim/dist/serve-sim.js"), "").unwrap();
        let mut tools = k.hub.device_tools();
        tools.globals = vec![global.clone()];
        k.hub.set_device_tools(tools);
        let s = k.hub.device_setup().unwrap();
        assert!(!s.serve_sim.installed && !s.ready, "{s:?}");
        assert_eq!(s.serve_sim.other.as_deref(), Some("0.0.1"));
        assert!(matches!(start(&k, BOOTED), Err(HubError::Conflict(_))));
        // The same pinned version installed globally is used.
        std::fs::write(global.join("serve-sim/package.json"), format!(r#"{{"version":"{SERVE_SIM_VERSION}"}}"#))
            .unwrap();
        assert!(k.hub.device_setup().unwrap().serve_sim.installed);
    }

    #[test]
    fn non_arm64_reports_unsupported() {
        let (k, fake) = kit(true);
        let mut tools = k.hub.device_tools();
        tools.arm64 = false;
        k.hub.set_device_tools(tools);
        let s = k.hub.device_setup().unwrap();
        assert!(!s.arm64 && !s.ready, "{s:?}");
        // Nothing else was even asked.
        assert!(!s.xcode && !s.node.found);
        assert!(!fake.join("calls").exists());
    }

    #[test]
    fn never_passes_allow_exec_or_detach() {
        let args = serve_sim_args(Path::new("/t/serve-sim.js"), 4100, BOOTED);
        assert_eq!(args, ["/t/serve-sim.js", "--no-preview", "-q", "-p", "4100", BOOTED]);
        let (k, fake) = kit(true);
        let g = start(&k, BOOTED).unwrap();
        let rec = records(&fake, "args");
        assert_eq!(rec.len(), 1);
        let got: Vec<&str> = rec[0].split_whitespace().collect();
        for bad in ["--detach", "-d", "--allow-exec", "--auth-token"] {
            assert!(!got.contains(&bad), "{bad} in {got:?}");
        }
        assert!(!rec[0].contains(&g.token), "the token is on the command line");
        assert_eq!(got, ["--no-preview", "-q", "-p", &g.port.to_string(), BOOTED]);
        k.hub.close_all_devices();
    }

    #[test]
    fn random_port_and_token_per_start() {
        let (k, _fake) = kit(true);
        let a = start(&k, BOOTED).unwrap();
        let b = start(&k, BOOTED_OLD).unwrap();
        assert_ne!(a.port, b.port);
        assert_ne!(a.token, b.token);
        for g in [&a, &b] {
            assert_eq!(g.token.len(), 64);
            assert!(g.token.bytes().all(|c| c.is_ascii_hexdigit()));
        }
        // A second viewer of a device shares its serve-sim; a fresh start after a kill gets
        // a new port and token.
        let a2 = start(&k, BOOTED).unwrap();
        assert_eq!((a2.port, &a2.token), (a.port, &a.token));
        assert_ne!(a2.stream, a.stream);
        k.hub.device_stop_server(BOOTED).unwrap();
        let a3 = start(&k, BOOTED).unwrap();
        assert_ne!(a3.token, a.token);
        assert!(k.hub.device_list().unwrap().iter().find(|d| d.udid == BOOTED).unwrap().streaming);
        k.hub.close_all_devices();
    }

    #[test]
    fn refcount_stops_after_grace() {
        let (k, _fake) = kit(true);
        k.hub.set_device_grace(Duration::from_millis(300));
        let a = start(&k, BOOTED).unwrap();
        let b = start(&k, BOOTED).unwrap();
        let up = |k: &Kit| k.hub.device_servers().iter().any(|(u, _, _)| u == BOOTED);
        k.hub.device_stream_stop(&a.stream).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert!(up(&k), "stopped while a viewer was left");
        k.hub.device_stream_stop(&b.stream).unwrap();
        assert!(up(&k), "stopped before the grace period");
        // A viewer back within the grace period keeps it (same port).
        let c = start(&k, BOOTED).unwrap();
        assert_eq!(c.port, a.port);
        std::thread::sleep(Duration::from_millis(600));
        assert!(up(&k), "the old timer stopped a viewed serve-sim");
        k.hub.device_stream_stop(&c.stream).unwrap();
        wait_for("the grace period", || !up(&k));
        assert!(k.hub.device_stream_stop(&c.stream).is_err());
    }

    #[test]
    fn memguard_reports_serve_sim_as_a_tool_and_stops_it() {
        let (k, _fake) = kit(true);
        assert!(k.hub.device_tool_views().is_empty());
        let g = start(&k, BOOTED).unwrap();
        let v = k.hub.device_tool_views();
        assert_eq!(v.len(), 1);
        assert_eq!(
            (v[0].kind.as_str(), v[0].name.as_str(), v[0].udid.as_str(), v[0].streams),
            ("tool", "serve-sim", BOOTED, 1)
        );
        assert!(v[0].bytes.unwrap_or(0) > 0, "{v:?}");
        // Never a live session (nothing to hibernate).
        assert!(k.hub.lock().live.is_empty());
        k.hub.device_stop_server(BOOTED).unwrap();
        assert!(k.hub.device_tool_views().is_empty());
        assert!(k.hub.device_stream_stop(&g.stream).is_err(), "its stream ended with it");
        assert!(k.hub.device_stop_server(BOOTED).is_err());
    }

    #[test]
    fn daemon_shutdown_kills_tree() {
        let (k, fake) = kit(true);
        let _g = start(&k, BOOTED).unwrap();
        let servers = k.hub.device_servers();
        let root = servers[0].1.expect("its process").pid;
        let kid: i32 = records(&fake, "kid")[0].trim().parse().unwrap();
        assert!(alive(root) && alive(kid));
        k.hub.shutdown();
        wait_for("serve-sim and its child to end", || !alive(root) && !alive(kid));
        assert!(k.hub.device_servers().is_empty());
        assert!(matches!(start(&k, BOOTED), Err(HubError::Conflict(_))));
    }

    #[test]
    fn archive_lets_go_of_its_streams() {
        let (k, _fake) = kit(true);
        k.hub.set_device_grace(Duration::from_millis(100));
        let t = k.create(AgentKind::Claude, "a").task;
        let u = k.create(AgentKind::Claude, "b").task;
        k.hub.device_stream_start(&DeviceStreamStart { udid: BOOTED.into(), task: Some(t.id.clone()) }).unwrap();
        k.hub.device_stream_start(&DeviceStreamStart { udid: BOOTED_OLD.into(), task: Some(u.id.clone()) }).unwrap();
        k.hub.archive(&t.id).unwrap();
        wait_for("the archived task's device to stop", || k.hub.device_servers().len() == 1);
        assert_eq!(k.hub.device_servers()[0].0, BOOTED_OLD);
        let r = k.hub.device_stream_start(&DeviceStreamStart { udid: BOOTED.into(), task: Some(t.id) });
        assert!(matches!(r, Err(HubError::Conflict(_))), "{r:?}");
        k.hub.close_all_devices();
    }

    #[test]
    fn env_is_cleared() {
        let env = serve_sim_env(Path::new("/Users/p"), "/n/bin:/usr/bin:/bin", Path::new("/m/sim/tmp"), "tok");
        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["HOME", "PATH", "TMPDIR", "SERVE_SIM_CONTROL", "SERVE_SIM_AUTH_TOKEN"]);
        let (k, fake) = kit(true);
        let g = start(&k, BOOTED).unwrap();
        let rec = records(&fake, "env");
        let vars: HashMap<&str, &str> = rec[0].lines().filter_map(|l| l.split_once('=')).collect();
        // What /bin/sh itself adds (PWD, SHLVL, OLDPWD, _) aside, nothing of the daemon's.
        let shell_own = ["PWD", "SHLVL", "OLDPWD", "_"];
        let mut names: Vec<&str> = vars.keys().copied().filter(|n| !shell_own.contains(n)).collect();
        names.sort();
        assert_eq!(names, ["HOME", "PATH", "SERVE_SIM_AUTH_TOKEN", "SERVE_SIM_CONTROL", "TMPDIR"], "{rec:?}");
        assert_eq!(vars["SERVE_SIM_AUTH_TOKEN"], g.token);
        assert_eq!(vars["SERVE_SIM_CONTROL"], "0");
        let tmp = k.hub.paths().home().join("sim/tmp");
        let tmp = std::fs::canonicalize(&tmp).unwrap();
        assert_eq!(Path::new(vars["TMPDIR"].trim_end_matches('/')), tmp);
        assert_eq!(std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(vars["PATH"].starts_with(&fake.display().to_string()), "node's folder first: {}", vars["PATH"]);
        k.hub.close_all_devices();
    }

    #[test]
    fn a_start_line_off_loopback_is_refused() {
        assert_eq!(parse_start_line(r#"{"port":4100,"url":"http://127.0.0.1:4100"}"#), Some(Ok(4100)));
        assert_eq!(parse_start_line(r#"{"port":4100,"url":"http://localhost:4100/x"}"#), Some(Ok(4100)));
        assert!(matches!(parse_start_line(r#"{"port":4100,"url":"http://0.0.0.0:4100"}"#), Some(Err(_))));
        assert!(matches!(parse_start_line(r#"{"port":4100,"url":"http://192.168.1.9:4100"}"#), Some(Err(_))));
        assert_eq!(parse_start_line("serve-sim starting…"), None);
        // A port nobody listens on fails the loopback check.
        // (Port 1, tcpmux: nothing listens there.)
        assert!(loopback_only(1, &[]).is_err());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(loopback_only(port, &lan_addrs()).is_ok());
    }

    #[test]
    fn pinned_version_and_ignore_scripts() {
        let (k, fake) = kit(false);
        let tools = k.hub.paths().home().join("tools");
        let args = install_args(&tools);
        assert_eq!(
            args,
            [
                "install",
                "--prefix",
                &tools.to_string_lossy(),
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--save-exact",
                "serve-sim@0.1.49"
            ]
        );
        let before = k.hub.device_setup().unwrap();
        assert!(!before.serve_sim.installed);
        assert_eq!(before.install_command, format!("{} {}", fake.join("npm").display(), args.join(" ")));
        let after = k.hub.device_tool_install().unwrap();
        assert!(after.serve_sim.installed && after.ready, "{after:?}");
        // npm ran exactly the command shown (its folder resolved).
        let ran = std::fs::read_to_string(fake.join("npm-args")).unwrap();
        assert!(ran.contains("--ignore-scripts") && ran.ends_with("serve-sim@0.1.49\n"), "{ran}");
        assert_eq!(std::fs::metadata(&tools).unwrap().permissions().mode() & 0o777, 0o700);
        // A wrong integrity or version: refused, and what was installed is removed.
        std::fs::write(fake.join("npm-integrity"), "sha512-AAAA").unwrap();
        let r = k.hub.device_tool_install();
        assert!(matches!(r, Err(HubError::Conflict(ref m)) if m.contains("integrity")), "{r:?}");
        assert!(!tools.join("node_modules/serve-sim").exists());
        std::fs::write(fake.join("npm-integrity"), SERVE_SIM_INTEGRITY).unwrap();
        std::fs::write(fake.join("npm-version"), "0.1.50").unwrap();
        assert!(k.hub.device_tool_install().is_err());
        assert!(!k.hub.device_setup().unwrap().serve_sim.installed);
    }
}
