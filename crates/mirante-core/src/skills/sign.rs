//! Signing skill manifests with OpenSSH's `ssh-keygen -Y` (SSHSIG, namespace
//! `mirante-skill`, an ed25519 key Mirante generates). The same format as git's ssh signing,
//! so a later registry can verify with an `allowed_signers` list.
//!
//! [`Signer`] and [`Verifier`] are traits so the library and the hub can be tested without
//! a key; [`SshKeygen`] is the one implementation. It runs `/usr/bin/ssh-keygen` (or
//! `$MIRANTE_SSH_KEYGEN`; never looked up on `PATH`) with a cleared environment
//! (`PATH=/usr/bin:/bin` only: no `SSH_AUTH_SOCK`, no agent), stdin piped (no tty, no
//! prompt), in its own process group, and a 10 s timeout covering its exit and the end of
//! its output, after which the group is killed (a process that left the group and still
//! holds the pipes is no longer waited for).
//!
//! Layout under the Mirante home: `keys/` (0700) holds `skills_ed25519`, its `.pub` and
//! `allowed_signers` (`mirante-local namespaces="mirante-skill" <public key>`), all 0600;
//! `tmp/` (0700) holds a signature for the moment `-Y verify` reads it.
//!
//! **Limit (same uid).** An agent that runs shell commands as the user can read the key and
//! sign anything. The signature catches edits made outside Mirante and gives versions
//! portable provenance; it is no boundary against a process running as the user.
//! Injection requires the trust ledger and the signature both.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{SkillError, NAMESPACE, PRINCIPAL};

/// The ssh-keygen used unless `MIRANTE_SSH_KEYGEN` names another.
pub const DEFAULT_SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";
/// The variable that overrides [`DEFAULT_SSH_KEYGEN`].
pub const SSH_KEYGEN_ENV: &str = "MIRANTE_SSH_KEYGEN";
const KEY: &str = "skills_ed25519";
const SIGNERS: &str = "allowed_signers";
const ARMOR: &str = "-----BEGIN SSH SIGNATURE-----";
/// Largest signature read back or verified.
const MAX_SIG: usize = 16 * 1024;

pub trait Signer {
    /// An armored SSHSIG over exactly `msg`.
    fn sign(&self, msg: &[u8]) -> Result<String, SkillError>;
    /// The signing key's fingerprint (`SHA256:…`).
    fn fingerprint(&self) -> Result<String, SkillError>;
}

pub trait Verifier {
    /// Ok when `sig` is a valid `mirante-skill` signature over exactly `msg` by `principal`
    /// made with the key whose fingerprint is `key` (`SHA256:…`).
    fn verify(&self, msg: &[u8], sig: &str, principal: &str, key: &str) -> Result<(), SkillError>;
}

fn sign_err(msg: impl Into<String>) -> SkillError {
    SkillError::Sign(msg.into())
}

/// `ssh-keygen` with Mirante's skill key.
#[derive(Debug, Clone)]
pub struct SshKeygen {
    pub bin: PathBuf,
    pub key: PathBuf,
    pub signers: PathBuf,
    pub tmp: PathBuf,
    pub timeout: Duration,
}

/// Creates `dir` 0700 if missing; refuses a symlink or a non-directory; tightens an
/// existing one to 0700.
fn private_dir(dir: &Path) -> Result<(), SkillError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(SkillError::Unsafe(format!("{} is a symlink or not a directory", dir.display())))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e.into()),
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Opens an existing regular file for reading without following a symlink.
fn open_plain(p: &Path) -> std::io::Result<File> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(p)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other(format!("{} is not a regular file", p.display())));
    }
    Ok(f)
}

fn exists_nofollow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

impl SshKeygen {
    /// The binary to run: `$MIRANTE_SSH_KEYGEN`, else [`DEFAULT_SSH_KEYGEN`].
    pub fn bin_from_env() -> PathBuf {
        std::env::var_os(SSH_KEYGEN_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_SSH_KEYGEN))
    }

    /// The signer for `home` (the Mirante home), without touching the disk.
    pub fn at(home: &Path, bin: &Path) -> SshKeygen {
        SshKeygen {
            bin: bin.to_path_buf(),
            key: home.join("keys").join(KEY),
            signers: home.join("keys").join(SIGNERS),
            tmp: home.join("tmp"),
            timeout: Duration::from_secs(10),
        }
    }

    /// [`SshKeygen::ensure_key_with`] with [`SshKeygen::bin_from_env`].
    pub fn ensure_key(home: &Path) -> Result<SshKeygen, SkillError> {
        SshKeygen::ensure_key_with(home, &SshKeygen::bin_from_env())
    }

    /// The signer for `home`, generating the key on first use: `keys/` and `tmp/` 0700,
    /// `ssh-keygen -q -t ed25519 -N "" -C mirante-skills -f keys/skills_ed25519`, and
    /// `allowed_signers` for [`PRINCIPAL`]; every file 0600. An existing key is reused, and
    /// its `allowed_signers` is never rewritten from whatever `.pub` is on disk: a missing
    /// one is written, one that names another key is an error. (A key, `.pub` and
    /// `allowed_signers` swapped together are consistent here: the caller compares
    /// [`Signer::fingerprint`] with the fingerprint it pinned when the key was made.)
    pub fn ensure_key_with(home: &Path, bin: &Path) -> Result<SshKeygen, SkillError> {
        let k = SshKeygen::at(home, bin);
        private_dir(&home.join("keys"))?;
        private_dir(&k.tmp)?;
        let public = k.public_key_path();
        let generated = !exists_nofollow(&k.key);
        if generated {
            if exists_nofollow(&public) {
                return Err(sign_err(format!("{} exists without its private key", public.display())));
            }
            let key = k.key.to_string_lossy().into_owned();
            k.run(&["-q", "-t", "ed25519", "-N", "", "-C", "mirante-skills", "-f", &key], b"")?;
        }
        for f in [&k.key, &public] {
            open_plain(f).map_err(|e| sign_err(format!("{}: {e}", f.display())))?;
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600))?;
        }
        let mut pubkey = String::new();
        open_plain(&public)?.take(4096).read_to_string(&mut pubkey)?;
        let fields: Vec<&str> = pubkey.split_whitespace().collect();
        let [kind @ "ssh-ed25519", blob, ..] = fields[..] else {
            return Err(sign_err("the skill key is not an ed25519 public key"));
        };
        let line = format!("{PRINCIPAL} namespaces=\"{NAMESPACE}\" {kind} {blob}\n");
        let current = open_plain(&k.signers).ok().and_then(|f| {
            let mut s = String::new();
            f.take(8192).read_to_string(&mut s).ok().map(|_| s)
        });
        match current {
            Some(c) if c == line => {}
            Some(_) if !generated => {
                return Err(sign_err(format!("{} does not name the skill key", k.signers.display())));
            }
            _ => write_private(&k.signers, line.as_bytes())?,
        }
        Ok(k)
    }

    fn public_key_path(&self) -> PathBuf {
        let mut p = self.key.clone().into_os_string();
        p.push(".pub");
        PathBuf::from(p)
    }

    /// Runs the binary with `args` and `input` on stdin; stdout on success.
    fn run(&self, args: &[&str], input: &[u8]) -> Result<Vec<u8>, SkillError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group: a timeout kills whatever it started too (a child holding
            // the pipes open would otherwise keep the readers waiting).
            .process_group(0);
        if self.tmp.is_dir() {
            cmd.current_dir(&self.tmp);
        }
        let mut child = cmd.spawn().map_err(|e| sign_err(format!("cannot run {}: {e}", self.bin.display())))?;
        let pgid = child.id() as libc::pid_t;
        // The writer and readers are detached: a process the binary started can hold the
        // pipes open past its exit (even outside its process group, after a setsid), and
        // joining a reader would then wait for it. They report on a channel instead, and
        // whatever has not reported by the deadline is a timeout.
        if let Some(mut w) = child.stdin.take() {
            let input = input.to_vec();
            std::thread::spawn(move || {
                let _ = w.write_all(&input);
            });
        }
        let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
        let read = |r: Option<Box<dyn Read + Send>>, is_out: bool, max: u64| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut b = Vec::new();
                if let Some(r) = r {
                    let _ = r.take(max).read_to_end(&mut b);
                }
                let _ = tx.send((is_out, b));
            });
        };
        read(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>), true, 1 << 20);
        read(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>), false, 64 * 1024);
        drop(tx);
        let timed_out = || {
            // SAFETY: plain syscall; the group is the child's own (process_group(0)).
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            sign_err(format!("{} timed out", self.bin.display()))
        };
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Ok(st),
                Ok(None) if Instant::now() >= deadline => {
                    let e = timed_out();
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(e);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => break Err(e.into()),
            }
        };
        let (mut out, mut err) = (None, None);
        if status.is_ok() {
            while out.is_none() || err.is_none() {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok((true, b)) => out = Some(b),
                    Ok((false, b)) => err = Some(b),
                    Err(_) => return Err(timed_out()),
                }
            }
        }
        let (out, err) = (out.unwrap_or_default(), err.unwrap_or_default());
        let status = status?;
        if !status.success() {
            let why = String::from_utf8_lossy(&err);
            return Err(sign_err(format!("ssh-keygen {}: {}", args.first().unwrap_or(&""), why.trim())));
        }
        Ok(out)
    }
}

/// Writes `bytes` to `p` (0600, never through a symlink): a fresh temp file beside it,
/// then a rename over it.
fn write_private(p: &Path, bytes: &[u8]) -> Result<(), SkillError> {
    let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(|| sign_err("bad file name"))?;
    let tmp = p.with_file_name(format!(".{name}.tmp-{}", uuid::Uuid::now_v7()));
    let mut f =
        OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
    let done = f.write_all(bytes).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&tmp, p));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// A principal safe to put on ssh-keygen's command line.
fn check_principal(p: &str) -> Result<(), SkillError> {
    let ok = !p.is_empty()
        && p.len() <= 128
        && !p.starts_with('-')
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'));
    if ok {
        Ok(())
    } else {
        Err(sign_err(format!("bad principal {p:?}")))
    }
}

impl Signer for SshKeygen {
    /// `ssh-keygen -Y sign -n mirante-skill -f <key>`, `msg` on stdin, the signature on stdout.
    fn sign(&self, msg: &[u8]) -> Result<String, SkillError> {
        let key = self.key.to_string_lossy().into_owned();
        let out = self.run(&["-Y", "sign", "-n", NAMESPACE, "-f", &key], msg)?;
        let sig = String::from_utf8(out).map_err(|_| sign_err("signature is not text"))?;
        if !sig.starts_with(ARMOR) || sig.len() > MAX_SIG {
            return Err(sign_err("ssh-keygen did not print a signature"));
        }
        Ok(sig)
    }

    /// `SHA256:…` from `ssh-keygen -l -f <key>.pub`.
    fn fingerprint(&self) -> Result<String, SkillError> {
        let public = self.public_key_path().to_string_lossy().into_owned();
        let out = self.run(&["-l", "-f", &public], b"")?;
        let text = String::from_utf8_lossy(&out);
        text.split_whitespace()
            .nth(1)
            .filter(|f| f.starts_with("SHA256:"))
            .map(str::to_string)
            .ok_or_else(|| sign_err("unexpected ssh-keygen -l output"))
    }
}

impl Verifier for SshKeygen {
    /// `ssh-keygen -Y verify -f <allowed_signers> -I <principal> -n mirante-skill -s <sig>`,
    /// `msg` on stdin; the signature is copied 0600 into `tmp/` for the call and removed.
    /// Its `Good "…" signature for <principal> with <TYPE> key <fingerprint>` line must name
    /// `key`.
    fn verify(&self, msg: &[u8], sig: &str, principal: &str, key: &str) -> Result<(), SkillError> {
        check_principal(principal)?;
        if !key.starts_with("SHA256:") || key.contains(char::is_whitespace) {
            return Err(sign_err("bad key fingerprint"));
        }
        if !sig.starts_with(ARMOR) || sig.len() > MAX_SIG {
            return Err(sign_err("not an SSH signature"));
        }
        let path = self.tmp.join(format!("sig-{}", uuid::Uuid::now_v7()));
        let mut f =
            OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path)?;
        let written = f.write_all(sig.as_bytes()).and_then(|()| f.sync_all());
        drop(f);
        let result = written.map_err(SkillError::from).and_then(|()| {
            let signers = self.signers.to_string_lossy().into_owned();
            let sig_path = path.to_string_lossy().into_owned();
            self.run(&["-Y", "verify", "-f", &signers, "-I", principal, "-n", NAMESPACE, "-s", &sig_path], msg)
        });
        let _ = std::fs::remove_file(&path);
        let out = result?;
        let text = String::from_utf8_lossy(&out);
        let signed_by = text.lines().find(|l| l.starts_with("Good ")).and_then(|l| l.split_whitespace().last());
        if signed_by != Some(key) {
            return Err(sign_err("signed with another key"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::os::unix::fs::PermissionsExt;

    /// The system ssh-keygen, or `None` (the test prints why it is skipped).
    fn keygen() -> Option<PathBuf> {
        let p = PathBuf::from(DEFAULT_SSH_KEYGEN);
        if p.exists() {
            Some(p)
        } else {
            eprintln!("skipped: {DEFAULT_SSH_KEYGEN} is not installed");
            None
        }
    }

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn sign_then_verify_round_trips_and_tamper_fails() {
        let Some(bin) = keygen() else { return };
        let d = TempDir::new("sk");
        let k = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        let sig = k.sign(b"manifest\n").unwrap();
        assert!(sig.starts_with("-----BEGIN SSH SIGNATURE-----"));
        k.verify(b"manifest\n", &sig, "mirante-local", &k.fingerprint().unwrap()).unwrap();
        let fp = k.fingerprint().unwrap();
        assert!(k.verify(b"manifest!\n", &sig, "mirante-local", &fp).is_err());
        assert!(k.verify(b"manifest\n", &sig, "someone-else", &fp).is_err());
        assert!(k.verify(b"manifest\n", "not a signature", "mirante-local", &fp).is_err());
        assert!(k.verify(b"manifest\n", &sig, "-oProxyCommand=x", &fp).is_err());
        assert!(k.fingerprint().unwrap().starts_with("SHA256:"));
        // The temporary signature files are removed.
        assert_eq!(std::fs::read_dir(d.path().join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn key_and_signers_are_private_and_created_once() {
        let Some(bin) = keygen() else { return };
        let d = TempDir::new("sk-once");
        let k = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        let key = std::fs::read(d.path().join("keys/skills_ed25519")).unwrap();
        for f in ["keys/skills_ed25519", "keys/skills_ed25519.pub", "keys/allowed_signers"] {
            assert_eq!(mode(&d.path().join(f)), 0o600, "{f}");
        }
        for dir in ["keys", "tmp"] {
            assert_eq!(mode(&d.path().join(dir)), 0o700, "{dir}");
        }
        let signers = std::fs::read_to_string(d.path().join("keys/allowed_signers")).unwrap();
        assert!(signers.starts_with("mirante-local namespaces=\"mirante-skill\" ssh-ed25519 "), "{signers}");
        let again = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        assert_eq!(std::fs::read(d.path().join("keys/skills_ed25519")).unwrap(), key, "reused, not regenerated");
        // A signature made before still verifies with the reopened signer.
        again.verify(b"m\n", &k.sign(b"m\n").unwrap(), PRINCIPAL, &k.fingerprint().unwrap()).unwrap();
    }

    #[test]
    fn allowed_signers_is_never_rewritten_from_whatever_key_is_on_disk() {
        let Some(bin) = keygen() else { return };
        let d = TempDir::new("sk-pin");
        let k = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        let fp = k.fingerprint().unwrap();
        let sig = k.sign(b"m\n").unwrap();
        // The signature verifies only for the key that made it.
        k.verify(b"m\n", &sig, PRINCIPAL, &fp).unwrap();
        assert!(k.verify(b"m\n", &sig, PRINCIPAL, "SHA256:another").is_err());
        // Another key planted in place of the skill key (private and public).
        let other = TempDir::new("sk-other");
        let o = SshKeygen::ensure_key_with(other.path(), &bin).unwrap();
        for f in ["skills_ed25519", "skills_ed25519.pub"] {
            std::fs::copy(other.path().join("keys").join(f), d.path().join("keys").join(f)).unwrap();
        }
        let signers = std::fs::read(d.path().join("keys/allowed_signers")).unwrap();
        assert!(SshKeygen::ensure_key_with(d.path(), &bin).is_err(), "a swapped key is refused");
        assert_eq!(std::fs::read(d.path().join("keys/allowed_signers")).unwrap(), signers, "not rewritten");
        // An allowed_signers edited to trust the planted key is refused too.
        std::fs::copy(other.path().join("keys/allowed_signers"), d.path().join("keys/allowed_signers")).unwrap();
        let fresh = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        assert_eq!(fresh.fingerprint().unwrap(), o.fingerprint().unwrap(), "consistent on disk: the pin decides");
    }

    #[test]
    fn a_symlinked_keys_dir_is_refused() {
        let Some(bin) = keygen() else { return };
        let d = TempDir::new("sk-sym");
        let elsewhere = TempDir::new("sk-elsewhere");
        std::os::unix::fs::symlink(elsewhere.path(), d.path().join("keys")).unwrap();
        assert!(matches!(SshKeygen::ensure_key_with(d.path(), &bin), Err(SkillError::Unsafe(_))));
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn another_namespace_does_not_verify() {
        let Some(bin) = keygen() else { return };
        let d = TempDir::new("sk-ns");
        let k = SshKeygen::ensure_key_with(d.path(), &bin).unwrap();
        let mut child = std::process::Command::new(&bin)
            .args(["-Y", "sign", "-n", "git", "-f"])
            .arg(d.path().join("keys/skills_ed25519"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"manifest\n").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let git_sig = String::from_utf8(out.stdout).unwrap();
        assert!(k.verify(b"manifest\n", &git_sig, PRINCIPAL, &k.fingerprint().unwrap()).is_err());
    }

    fn fake(dir: &Path, script: &str) -> PathBuf {
        let bin = dir.join("fake-keygen");
        // Warmed up once, so macOS's first-exec scan is not charged to the signer timeouts.
        crate::test_script::write_sh(&bin, script, 0o700);
        bin
    }

    #[test]
    fn runs_with_a_clean_environment() {
        let d = TempDir::new("sk-env");
        let rec = d.path().join("env.txt");
        let args = d.path().join("args.txt");
        let bin = fake(
            d.path(),
            &format!(
                "env > '{}'\necho \"$@\" > '{}'\ncat > /dev/null\n\
                 echo '-----BEGIN SSH SIGNATURE-----'\necho 'eA=='\necho '-----END SSH SIGNATURE-----'\n",
                rec.display(),
                args.display()
            ),
        );
        let k = SshKeygen::at(d.path(), &bin);
        assert!(k.sign(b"m\n").unwrap().starts_with("-----BEGIN SSH SIGNATURE-----"));
        let env = std::fs::read_to_string(&rec).unwrap();
        let vars: Vec<&str> = env.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
        assert!(env.lines().any(|l| l == "PATH=/usr/bin:/bin"), "{env}");
        for leaked in ["SSH_AUTH_SOCK", "HOME", "MIRANTE_HOME", "SSH_AGENT_PID"] {
            assert!(!vars.contains(&leaked), "{leaked} leaked: {env}");
        }
        let argv = std::fs::read_to_string(&args).unwrap();
        let key = d.path().join("keys/skills_ed25519");
        assert_eq!(argv.trim(), format!("-Y sign -n mirante-skill -f {}", key.display()));
    }

    #[test]
    fn a_hung_signer_is_killed_and_a_missing_one_is_an_error() {
        let d = TempDir::new("sk-hang");
        let bin = fake(d.path(), "sleep 30\n");
        let mut k = SshKeygen::at(d.path(), &bin);
        k.timeout = Duration::from_millis(300);
        let t = Instant::now();
        assert!(matches!(k.sign(b"m\n"), Err(SkillError::Sign(e)) if e.contains("timed out")));
        assert!(t.elapsed() < Duration::from_secs(5));
        let missing = SshKeygen::at(d.path(), &d.path().join("nope"));
        assert!(matches!(missing.sign(b"m\n"), Err(SkillError::Sign(_))));
        assert!(SshKeygen::ensure_key_with(d.path(), &d.path().join("nope")).is_err());
    }

    #[test]
    fn a_process_left_holding_the_pipes_does_not_hang_the_caller() {
        let d = TempDir::new("sk-pipes");
        let sig = "echo '-----BEGIN SSH SIGNATURE-----'\necho 'eA=='\necho '-----END SSH SIGNATURE-----'\n";
        let mut scripts = vec![format!("{sig}sleep 30 &\nexit 0\n")];
        // One that also leaves the process group (setsid), where a group kill cannot reach.
        if Path::new("/usr/bin/perl").exists() {
            scripts.push(format!("{sig}/usr/bin/perl -e 'use POSIX; POSIX::setsid(); sleep 30' &\nexit 0\n"));
        }
        for script in scripts {
            let bin = fake(d.path(), &script);
            let mut k = SshKeygen::at(d.path(), &bin);
            k.timeout = Duration::from_millis(500);
            let t = Instant::now();
            let r = k.sign(b"m\n");
            assert!(t.elapsed() < Duration::from_secs(5), "{script}: took {:?}", t.elapsed());
            assert!(matches!(&r, Err(SkillError::Sign(e)) if e.contains("timed out")), "{script}: {r:?}");
        }
    }
}
