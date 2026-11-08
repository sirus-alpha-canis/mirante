//! Measuring memory: one agent's process tree and the whole machine.
//!
//! macOS: a process's memory is its `phys_footprint` (`proc_pid_rusage`, `RUSAGE_INFO_V4`),
//! the number Activity Monitor shows (RSS undercounts compressed and swapped pages). The
//! system side reads `hw.memsize`, `vm.swapusage`, the kernel's own pressure level
//! (`kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical) and a conservative
//! "free" (`vm.page_free_count + vm.page_speculative_count + vm.page_purgeable_count`, times
//! `hw.pagesize`; inactive file cache is not counted, it is what the policy judges by) and
//! what is available (`kern.memorystatus_level`, the system-wide free percentage that
//! `memory_pressure` prints, times `hw.memsize`: the file cache macOS gives back counts).
//! Linux: `Pss` from `/proc/<pid>/smaps_rollup` (else `VmRSS` from `/proc/<pid>/status`),
//! `/proc/meminfo` (`MemTotal`, `MemAvailable`, swap) and PSI `/proc/pressure/memory`.
use std::collections::HashMap;
use std::sync::Mutex;

use crate::driver::proctree::{descendants, Proc};

/// Memory of one process tree (root plus descendants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeSample {
    pub bytes: u64,
    pub procs: usize,
}

/// The raw pressure signal of the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemSample {
    pub total: u64,
    pub free: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// What a new program could get without swapping: macOS `kern.memorystatus_level` (the
    /// free percentage `memory_pressure` reports) times the RAM, Linux `MemAvailable`.
    /// `None` when the OS gave no such number (then only `free` is known).
    pub available: Option<u64>,
    pub pressure: Pressure,
    /// `pressure` came from the kernel's own measure (macOS memorystatus level, Linux PSI).
    /// Then swap and free are shown but not used to judge: macOS keeps swap allocated and
    /// free pages near zero (file cache) on a machine that is not under pressure.
    pub kernel_signal: bool,
}

/// `pct` percent of `total` (macOS `kern.memorystatus_level`); `None` for a value that is
/// no percentage.
pub fn available_from_level(pct: u64, total: u64) -> Option<u64> {
    (pct <= 100).then(|| total / 100 * pct)
}

impl SystemSample {
    /// Swap in use, in percent of the swap that exists (0 without swap).
    pub fn swap_pct(&self) -> u64 {
        (self.swap_used * 100).checked_div(self.swap_total).unwrap_or(0)
    }
}

pub trait MemSampler: Send + Sync {
    /// The tree under `root` (`None` when `root` is gone).
    fn tree(&self, root: Proc) -> Option<TreeSample>;
    fn system(&self) -> SystemSample;
}

/// One process's memory as this OS counts it (`None`: gone or not readable).
pub fn process_bytes(pid: i32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    return mac::footprint(pid);
    #[cfg(target_os = "linux")]
    return linux::process_bytes(pid);
}

/// Sums `bytes_of` over `root` and its live descendants.
fn sum_tree(root: Proc, bytes_of: impl Fn(i32) -> Option<u64>) -> Option<TreeSample> {
    let mut s = TreeSample { bytes: bytes_of(root.pid)?, procs: 1 };
    for p in descendants(root) {
        if let Some(b) = bytes_of(p.pid) {
            s.bytes += b;
            s.procs += 1;
        }
    }
    Some(s)
}

#[cfg(target_os = "macos")]
pub struct MacSampler;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_void, CString};
    use std::mem::{size_of, zeroed};

    /// `phys_footprint` of one process.
    pub fn footprint(pid: i32) -> Option<u64> {
        if pid <= 0 {
            return None;
        }
        // SAFETY: the buffer is a `rusage_info_v4`, the flavor asks for exactly that struct.
        let mut info: libc::rusage_info_v4 = unsafe { zeroed() };
        let r = unsafe {
            libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t)
        };
        (r == 0).then_some(info.ri_phys_footprint)
    }

    /// A fixed-size value from `sysctlbyname`.
    pub fn sysctl<T: Copy>(name: &str) -> Option<T> {
        let c = CString::new(name).ok()?;
        // SAFETY: `out` has the size passed; the kernel writes at most that many bytes.
        let mut out: T = unsafe { zeroed() };
        let mut len = size_of::<T>();
        let r = unsafe {
            libc::sysctlbyname(c.as_ptr(), &mut out as *mut T as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        (r == 0 && len == size_of::<T>()).then_some(out)
    }

    /// An integer sysctl that may be 32 or 64 bits wide.
    pub fn sysctl_int(name: &str) -> Option<u64> {
        sysctl::<u64>(name).or_else(|| sysctl::<u32>(name).map(u64::from))
    }
}

#[cfg(target_os = "macos")]
impl MemSampler for MacSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        if !crate::driver::proctree::alive(root) {
            return None;
        }
        sum_tree(root, process_bytes)
    }

    fn system(&self) -> SystemSample {
        let page = mac::sysctl_int("hw.pagesize").unwrap_or(4096);
        let pages: u64 = ["vm.page_free_count", "vm.page_speculative_count", "vm.page_purgeable_count"]
            .iter()
            .filter_map(|n| mac::sysctl_int(n))
            .sum();
        let swap = mac::sysctl::<libc::xsw_usage>("vm.swapusage");
        let level = mac::sysctl_int("kern.memorystatus_vm_pressure_level");
        let pressure = match level {
            Some(4) => Pressure::Critical,
            Some(2) => Pressure::Warn,
            _ => Pressure::Normal,
        };
        let total = mac::sysctl_int("hw.memsize").unwrap_or(0);
        SystemSample {
            total,
            free: pages * page,
            swap_used: swap.map_or(0, |s| s.xsu_used),
            swap_total: swap.map_or(0, |s| s.xsu_total),
            available: mac::sysctl_int("kern.memorystatus_level").and_then(|pct| available_from_level(pct, total)),
            pressure,
            kernel_signal: level.is_some(),
        }
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxSampler;

#[cfg(target_os = "linux")]
mod linux {
    /// The `kB` value of `key:` in a `/proc` file of `key: value kB` lines, in bytes.
    pub fn kb(text: &str, key: &str) -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|k| k * 1024)
    }

    /// Pss (proportional set size) when readable, else VmRSS.
    pub fn process_bytes(pid: i32) -> Option<u64> {
        if pid <= 0 {
            return None;
        }
        if let Some(pss) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok().and_then(|t| kb(&t, "Pss"))
        {
            return Some(pss);
        }
        kb(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?, "VmRSS")
    }

    /// `some avg10` of PSI memory (percent of time some task stalled on memory).
    pub fn psi_some_avg10(text: &str) -> Option<f64> {
        let line = text.lines().find(|l| l.starts_with("some "))?;
        line.split_whitespace().find_map(|f| f.strip_prefix("avg10="))?.parse().ok()
    }
}

#[cfg(target_os = "linux")]
impl MemSampler for LinuxSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        if !crate::driver::proctree::alive(root) {
            return None;
        }
        sum_tree(root, process_bytes)
    }

    fn system(&self) -> SystemSample {
        let mem = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let swap_total = linux::kb(&mem, "SwapTotal").unwrap_or(0);
        let psi = std::fs::read_to_string("/proc/pressure/memory").ok().and_then(|t| linux::psi_some_avg10(&t));
        let pressure = match psi {
            Some(p) if p >= 40.0 => Pressure::Critical,
            Some(p) if p >= 10.0 => Pressure::Warn,
            _ => Pressure::Normal,
        };
        SystemSample {
            total: linux::kb(&mem, "MemTotal").unwrap_or(0),
            free: linux::kb(&mem, "MemAvailable").unwrap_or(0),
            available: linux::kb(&mem, "MemAvailable"),
            swap_used: swap_total.saturating_sub(linux::kb(&mem, "SwapFree").unwrap_or(0)),
            swap_total,
            pressure,
            kernel_signal: psi.is_some(),
        }
    }
}

/// The sampler for this operating system.
pub fn os_sampler() -> Box<dyn MemSampler> {
    #[cfg(target_os = "macos")]
    return Box::new(MacSampler);
    #[cfg(target_os = "linux")]
    return Box::new(LinuxSampler);
}

/// Test double: bytes per root pid and a fixed system sample, both settable.
#[derive(Default)]
pub struct FakeSampler {
    pub trees: Mutex<HashMap<i32, TreeSample>>,
    pub sys: Mutex<SystemSample>,
}

impl FakeSampler {
    pub fn set_tree(&self, pid: i32, bytes: u64) {
        self.trees.lock().unwrap().insert(pid, TreeSample { bytes, procs: 1 });
    }

    pub fn set_system(&self, s: SystemSample) {
        *self.sys.lock().unwrap() = s;
    }
}

impl MemSampler for FakeSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        self.trees.lock().unwrap().get(&root.pid).copied()
    }

    fn system(&self) -> SystemSample {
        *self.sys.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::proctree::proc_of;

    #[test]
    fn our_own_tree_has_a_sane_footprint() {
        let me = proc_of(std::process::id() as i32).unwrap();
        let s = os_sampler().tree(me).unwrap();
        // A test binary: more than 1 MiB, far less than 64 GiB.
        assert!(s.bytes > 1 << 20 && s.bytes < 64 << 30, "{s:?}");
        assert!(s.procs >= 1);
    }

    #[test]
    fn a_tree_weighs_at_least_its_root() {
        let mut child =
            std::process::Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30 & wait"]).spawn().unwrap();
        let root = proc_of(child.id() as i32).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while descendants(root).len() < 2 {
            assert!(std::time::Instant::now() < deadline, "children never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let sampler = os_sampler();
        let tree = sampler.tree(root).unwrap();
        let alone = process_bytes(root.pid).unwrap();
        assert!(tree.procs >= 3, "{tree:?}");
        assert!(tree.bytes > alone, "{tree:?} vs root alone {alone}");
        let r =
            crate::driver::proctree::kill_tree(root, std::time::Duration::from_secs(2), &crate::driver::proctree::Kill);
        assert!(r.survivors.is_empty());
        let _ = child.wait();
        assert_eq!(sampler.tree(root), None, "a gone root has no sample");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_sample_matches_the_kernel() {
        let s = MacSampler.system();
        assert_eq!(Some(s.total), mac::sysctl_int("hw.memsize"));
        assert!(s.free > 0 && s.free < s.total, "{s:?}");
        assert!(s.swap_used <= s.swap_total || s.swap_total == 0, "{s:?}");
        let raw = mac::sysctl_int("kern.memorystatus_vm_pressure_level").unwrap();
        assert!([1, 2, 4].contains(&raw), "pressure level {raw}");
        assert!(s.kernel_signal);
        // Available counts what macOS gives back (file cache): never less than the strict free.
        let pct = mac::sysctl_int("kern.memorystatus_level").unwrap();
        assert!(pct <= 100, "{pct}");
        let avail = s.available.expect("macOS reports its free percentage");
        assert!(avail <= s.total, "{s:?}");
    }

    #[test]
    fn available_is_the_free_percentage_of_the_ram() {
        const GB: u64 = 1 << 30;
        // The founder's screenshot: 45 % free of 16 GB is about 7.2 GB, not "454 MB free".
        assert_eq!(available_from_level(45, 16 * GB), Some(16 * GB / 100 * 45));
        assert_eq!(available_from_level(0, 16 * GB), Some(0));
        assert_eq!(available_from_level(101, 16 * GB), None);
    }

    #[test]
    fn the_fake_returns_what_it_was_given() {
        let f = FakeSampler::default();
        let p = Proc { pid: 42, start: 1 };
        assert_eq!(f.tree(p), None);
        f.set_tree(42, 7);
        assert_eq!(f.tree(p), Some(TreeSample { bytes: 7, procs: 1 }));
        f.set_system(SystemSample { total: 100, swap_used: 3, swap_total: 4, ..Default::default() });
        assert_eq!(f.system().swap_pct(), 75);
        assert_eq!(SystemSample::default().swap_pct(), 0);
    }
}
