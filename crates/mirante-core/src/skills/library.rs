//! The versioned skill library: `<lib>/<name>/v<N>/` folders (`<lib>` is
//! `$MIRANTE_HOME/skills`), each holding `SKILL.md`, optional text files, the manifest
//! (`mirante-skill.json`) and its signature (`mirante-skill.sig`).
//!
//! Version folders are create-only: a version is written into a `.stage-<uuid>` folder
//! through [`SafeDir`] (files `O_CREAT | O_EXCL | O_NOFOLLOW`, 0600, fsynced), then renamed
//! to `v<N>` with a rename that never replaces (an existing `v<N>`, even empty, makes the
//! write fail), all under an exclusive lock on `<lib>/<name>`. Nothing is ever overwritten;
//! old versions stay for diff and rollback until [`prune`] keeps the newest ones.
//!
//! [`read_verified`] is the check before every injection: the manifest's bytes must hash to
//! the hash a person accepted (the ledger's), the folder must hold exactly the manifest's
//! files plus the manifest and the signature (no symlinks, no hard links), each file must
//! match its pinned hash and size, the manifest must name the pinned signing key, and the
//! signature must verify with that key.
use std::path::{Path, PathBuf};

use super::format::{check_file_path, is_name, FileEntry, Manifest, SignerRef};
use super::sign::{Signer, Verifier};
use super::{SkillError, MANIFEST, MAX_BODY, MAX_FILE, PRINCIPAL, SIGNATURE, SKILL_MD};
use crate::dna::sha256_hex;
use crate::safedir::{SafeDir, SafeError};

/// Largest manifest or signature read back.
const MAX_META: usize = 64 * 1024;
/// How deep a version folder's files may nest (`a/b/c.md`).
const MAX_LEVELS: usize = 3;
/// Most entries in one folder of a version, and most files in a version.
const MAX_ENTRIES: usize = 64;

impl From<SafeError> for SkillError {
    fn from(e: SafeError) -> Self {
        match e {
            SafeError::Unsafe(m) => SkillError::Unsafe(m),
            SafeError::Exists(m) => SkillError::Exists(m),
            SafeError::Missing(m) => SkillError::Missing(m),
            e @ (SafeError::TooLarge(..) | SafeError::NotUtf8(_)) => SkillError::Modified(e.to_string()),
            SafeError::Io(m) => SkillError::Io(m),
        }
    }
}

/// What to write as a new version: the manifest (its `files` and `signer` are filled in by
/// [`write_version`]) and the files, `SKILL.md` first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub manifest: Manifest,
    pub files: Vec<(String, Vec<u8>)>,
}

/// A version just written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub version: u32,
    pub manifest: Manifest,
    /// What the trust ledger records.
    pub manifest_hash: String,
    pub path: PathBuf,
}

/// A version read back and verified: its manifest and its files' bytes, in manifest order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSkill {
    pub manifest: Manifest,
    pub manifest_hash: String,
    pub files: Vec<(String, Vec<u8>)>,
}

/// The folder name of a version.
pub fn version_dir(v: u32) -> String {
    format!("v{v}")
}

/// `v<N>` (no sign, no leading zero) → N.
fn parse_version_dir(s: &str) -> Option<u32> {
    let digits = s.strip_prefix('v')?;
    let ok = !digits.is_empty() && digits.len() <= 9 && !digits.starts_with('0');
    ok.then(|| digits.parse().ok()).flatten().filter(|v| *v > 0)
}

/// `lib`'s parent and last component: the library is opened from its parent, so a
/// symlinked library directory is refused like any other level.
fn split_lib(lib: &Path) -> Result<(&Path, &str), SkillError> {
    let parent = lib.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = lib.file_name().and_then(|n| n.to_str()).ok_or_else(|| SkillError::Io("bad library path".into()))?;
    Ok((parent, name))
}

fn check_name(name: &str) -> Result<(), SkillError> {
    if is_name(name) {
        Ok(())
    } else {
        Err(SkillError::Format(format!("{name:?} is not a skill name")))
    }
}

/// The directory holding `path`'s file inside `root` (descriptor-relative, level by level;
/// made with 0700 when `create`), and the file's name. `None` when a level is missing.
fn parent_of(root: &SafeDir, path: &str, create: bool) -> Result<Option<(SafeDir, String)>, SkillError> {
    let mut segs: Vec<&str> = path.split('/').collect();
    let file = segs.pop().unwrap_or_default().to_string();
    let mut dir = root.try_clone()?;
    for seg in segs {
        dir = if create {
            dir.ensure_dir(seg, 0o700)?
        } else {
            match dir.open_dir(seg)? {
                Some(d) => d,
                None => return Ok(None),
            }
        };
    }
    Ok(Some((dir, file)))
}

/// Writes `b` as a new version of its skill (see the module docs). The manifest's `files`
/// are computed from the bundle (extras sorted by path) and its `signer` set to
/// [`PRINCIPAL`] and the signer's fingerprint; the result must pass
/// [`Manifest::validate`]. [`SkillError::Exists`] when that version exists.
pub fn write_version(lib: &Path, signer: &dyn Signer, b: &Bundle) -> Result<Written, SkillError> {
    let name = b.manifest.name.as_str();
    check_name(name)?;
    let (first, extras) = b.files.split_first().ok_or_else(|| SkillError::Format("no files".into()))?;
    if first.0 != SKILL_MD {
        return Err(SkillError::Format("the first file must be SKILL.md".into()));
    }
    let mut extras: Vec<&(String, Vec<u8>)> = extras.iter().collect();
    extras.sort_by(|x, y| x.0.cmp(&y.0));
    for (p, _) in &extras {
        check_file_path(p)?;
    }
    let mut manifest = b.manifest.clone();
    manifest.files =
        std::iter::once(first).chain(extras.iter().copied()).map(|(p, bytes)| FileEntry::of(p, bytes)).collect();
    manifest.signer = SignerRef { principal: PRINCIPAL.into(), key: signer.fingerprint()? };
    manifest.validate()?;
    let bytes = manifest.to_bytes();
    let sig = signer.sign(&bytes)?;
    let (parent, lib_name) = split_lib(lib)?;
    let lib_dir = SafeDir::create(parent, &[lib_name], 0o700)?;
    let skill_dir = lib_dir.ensure_dir(name, 0o700)?;
    let _lock = skill_dir.lock()?;
    let target = version_dir(manifest.version);
    if skill_dir.stat(&target)?.is_some() {
        return Err(SkillError::Exists(skill_dir.path().join(&target).display().to_string()));
    }
    let stage_name = format!(".stage-{}", uuid::Uuid::now_v7());
    let stage = skill_dir.create_dir_new(&stage_name, 0o700)?;
    let staged = (|| -> Result<(), SkillError> {
        let all = std::iter::once(first).chain(extras.iter().copied());
        for (path, data) in all {
            let (dir, file) = parent_of(&stage, path, true)?.ok_or_else(|| SkillError::Io("staging".into()))?;
            dir.write_new(&file, data, 0o600)?;
            dir.sync()?;
        }
        stage.write_new(MANIFEST, &bytes, 0o600)?;
        stage.write_new(SIGNATURE, sig.as_bytes(), 0o600)?;
        stage.sync()?;
        skill_dir.rename_new(&stage_name, &target)?;
        skill_dir.sync()?;
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = skill_dir.remove_tree(&stage_name);
        return Err(e);
    }
    Ok(Written {
        version: manifest.version,
        manifest_hash: manifest.hash(),
        path: skill_dir.path().join(&target),
        manifest,
    })
}

/// The regular files under `dir`, as `/`-separated paths below `prefix`. A symlink or any
/// other non-file, non-directory entry is [`SkillError::Modified`], and so is a folder with
/// more than [`MAX_ENTRIES`] entries or a version with more than that many files: never a
/// listing cut short, which would leave entries past the cut unseen.
fn walk(dir: &SafeDir, prefix: &str, depth: usize, out: &mut Vec<String>) -> Result<(), SkillError> {
    if depth > MAX_LEVELS {
        return Err(SkillError::Modified(format!("{prefix} is nested too deep")));
    }
    let entries = dir.entries(MAX_ENTRIES)?;
    if entries.len() > MAX_ENTRIES {
        let at = if prefix.is_empty() { "the version folder" } else { prefix };
        return Err(SkillError::Modified(format!("{at} has too many entries")));
    }
    for e in entries {
        let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
        if out.len() >= MAX_ENTRIES {
            return Err(SkillError::Modified("too many files".into()));
        }
        if e.is_dir() {
            let sub = dir.open_dir(&e.name)?.ok_or_else(|| SkillError::Modified(format!("{path} vanished")))?;
            walk(&sub, &path, depth + 1, out)?;
        } else if e.is_file() {
            out.push(path);
        } else {
            return Err(SkillError::Modified(format!("{path} is not a regular file")));
        }
    }
    Ok(())
}

/// Reads version `version` of `name` and verifies it against `expect_hash` (the manifest
/// hash a person accepted), `signer_key` (the fingerprint of the signing key Mirante
/// pinned: the manifest must name it and the signature must be made with it) and the
/// signature (see the module docs). Any difference is [`SkillError::Modified`] naming what
/// changed.
pub fn read_verified(
    lib: &Path,
    name: &str,
    version: u32,
    expect_hash: &str,
    signer_key: &str,
    verifier: &dyn Verifier,
) -> Result<VerifiedSkill, SkillError> {
    check_name(name)?;
    let (parent, lib_name) = split_lib(lib)?;
    let vdir = version_dir(version);
    let dir = SafeDir::open(parent, &[lib_name, name, &vdir])?
        .ok_or_else(|| SkillError::Missing(lib.join(name).join(&vdir).display().to_string()))?;
    let modified = |what: String| SkillError::Modified(what);
    let bytes = dir.read_bytes(MANIFEST, MAX_META)?;
    if sha256_hex(&bytes) != expect_hash {
        return Err(modified(format!("{MANIFEST} is not the reviewed one")));
    }
    let manifest = Manifest::parse(&bytes).map_err(|e| modified(format!("{MANIFEST}: {e}")))?;
    if manifest.name != name || manifest.version != version {
        return Err(modified(format!("{MANIFEST} names another skill or version")));
    }
    if manifest.signer.key != signer_key {
        return Err(modified(format!("{MANIFEST} was signed by another key than Mirante's")));
    }
    let mut listed = Vec::new();
    walk(&dir, "", 0, &mut listed)?;
    listed.sort();
    let mut expected: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
    expected.extend([MANIFEST.to_string(), SIGNATURE.to_string()]);
    expected.sort();
    if let Some(extra) = listed.iter().find(|p| !expected.contains(p)) {
        return Err(modified(format!("unexpected file {extra}")));
    }
    if let Some(missing) = expected.iter().find(|p| !listed.contains(p)) {
        return Err(modified(format!("missing file {missing}")));
    }
    let mut files = Vec::with_capacity(manifest.files.len());
    for f in &manifest.files {
        let (d, file) = parent_of(&dir, &f.path, false)?.ok_or_else(|| modified(format!("missing file {}", f.path)))?;
        let max = if f.path == SKILL_MD { MAX_BODY + 4096 } else { MAX_FILE };
        let data = d.read_bytes(&file, max).map_err(|e| match e {
            SafeError::Unsafe(m) => SkillError::Unsafe(m),
            other => modified(format!("{}: {other}", f.path)),
        })?;
        if data.len() as u64 != f.bytes || sha256_hex(&data) != f.sha256 {
            return Err(modified(f.path.clone()));
        }
        files.push((f.path.clone(), data));
    }
    let sig = dir.read_file(SIGNATURE, MAX_META).map_err(|e| modified(format!("{SIGNATURE}: {e}")))?;
    verifier
        .verify(&bytes, &sig, &manifest.signer.principal, &manifest.signer.key)
        .map_err(|e| modified(format!("signature does not verify: {e}")))?;
    Ok(VerifiedSkill { manifest, manifest_hash: expect_hash.to_string(), files })
}

/// The versions of `name` on disk, ascending; empty when it has none.
pub fn list_versions(lib: &Path, name: &str) -> Result<Vec<u32>, SkillError> {
    check_name(name)?;
    let (parent, lib_name) = split_lib(lib)?;
    let Some(dir) = SafeDir::open(parent, &[lib_name, name])? else {
        return Ok(Vec::new());
    };
    let mut out: Vec<u32> =
        dir.entries(10_000)?.into_iter().filter(|e| e.is_dir()).filter_map(|e| parse_version_dir(&e.name)).collect();
    out.sort_unstable();
    Ok(out)
}

/// Removes version `version` of `name` (a version just written whose acceptance the store
/// then refused), through descriptors, under the skill's lock. A missing one is Ok.
pub fn remove_version(lib: &Path, name: &str, version: u32) -> Result<(), SkillError> {
    check_name(name)?;
    let (parent, lib_name) = split_lib(lib)?;
    let Some(dir) = SafeDir::open(parent, &[lib_name, name])? else {
        return Ok(());
    };
    let _lock = dir.lock()?;
    let target = version_dir(version);
    if dir.stat(&target)?.is_some() {
        dir.remove_tree(&target)?;
        dir.sync()?;
    }
    Ok(())
}

/// Keeps the newest `keep` versions of `name` and removes the older ones (and staging
/// folders a crashed write left), through descriptors, under the skill's lock. A version in
/// `protect` (the ledger's current one) is never removed. Returns the versions removed.
pub fn prune(lib: &Path, name: &str, keep: usize, protect: &[u32]) -> Result<Vec<u32>, SkillError> {
    check_name(name)?;
    let (parent, lib_name) = split_lib(lib)?;
    let Some(dir) = SafeDir::open(parent, &[lib_name, name])? else {
        return Ok(Vec::new());
    };
    let _lock = dir.lock()?;
    let entries = dir.entries(10_000)?;
    for e in entries.iter().filter(|e| e.is_dir() && e.name.starts_with(".stage-")) {
        dir.remove_tree(&e.name)?;
    }
    let mut versions: Vec<u32> =
        entries.iter().filter(|e| e.is_dir()).filter_map(|e| parse_version_dir(&e.name)).collect();
    versions.sort_unstable();
    let old = versions.len().saturating_sub(keep);
    let removed: Vec<u32> = versions[..old].iter().copied().filter(|v| !protect.contains(v)).collect();
    for v in &removed {
        dir.remove_tree(&version_dir(*v))?;
    }
    if !removed.is_empty() {
        dir.sync()?;
    }
    Ok(removed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::dna::sha256_hex;
    use crate::skills::format::{render_skill_md, Activation, Provenance, SignerRef, Source};
    use crate::skills::origin::Scope;
    use crate::skills::sign::{SshKeygen, DEFAULT_SSH_KEYGEN};
    use crate::skills::{FORMAT, KEEP_VERSIONS, PRINCIPAL};
    use crate::test_support::TempDir;

    /// Signs with a hash, verifies by recomputing it: the library's logic without a key.
    struct Fake;

    impl Signer for Fake {
        fn sign(&self, msg: &[u8]) -> Result<String, SkillError> {
            Ok(format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", sha256_hex(msg)))
        }
        fn fingerprint(&self) -> Result<String, SkillError> {
            Ok("SHA256:fake".into())
        }
    }

    impl Verifier for Fake {
        fn verify(&self, msg: &[u8], sig: &str, principal: &str, key: &str) -> Result<(), SkillError> {
            if principal == PRINCIPAL && key == "SHA256:fake" && sig == self.sign(msg)? {
                Ok(())
            } else {
                Err(SkillError::Sign("bad signature".into()))
            }
        }
    }

    const FAKE_KEY: &str = "SHA256:fake";

    fn signer() -> Fake {
        Fake
    }

    fn verifier() -> Fake {
        Fake
    }

    /// `<home>/skills` (not created yet: write_version makes it).
    fn lib_dir() -> (TempDir, PathBuf) {
        let home = TempDir::new("skills-lib");
        let lib = home.path().join("skills");
        (home, lib)
    }

    fn bundle(name: &str, version: u32) -> Bundle {
        Bundle {
            manifest: Manifest {
                format: FORMAT.into(),
                name: name.into(),
                version,
                description: "Run the tests first".into(),
                activation: Activation::Always,
                agents: vec!["claude".into(), "codex".into()],
                scope: Scope::org("github.com/acme"),
                files: Vec::new(),
                parent: None,
                provenance: Provenance { source: Source::Cli, task: None, proposal: None, learned_in: None },
                signer: SignerRef { principal: String::new(), key: String::new() },
                created: "2026-11-08".into(),
            },
            files: vec![
                (
                    SKILL_MD.into(),
                    render_skill_md(name, "Run the tests first", "Run `cargo test` first.\n").into_bytes(),
                ),
                ("reference/guide.md".into(), b"Guide\n".to_vec()),
            ],
        }
    }

    #[test]
    fn write_version_is_create_only_and_reads_back_verified() {
        let (_home, lib) = lib_dir();
        let v = write_version(&lib, &signer(), &bundle("a", 1)).unwrap();
        assert_eq!(v.version, 1);
        assert_eq!(v.manifest.signer, SignerRef { principal: PRINCIPAL.into(), key: "SHA256:fake".into() });
        assert_eq!(v.manifest_hash, v.manifest.hash());
        assert!(write_version(&lib, &signer(), &bundle("a", 1)).is_err()); // v1 exists
        let r = read_verified(&lib, "a", 1, &v.manifest_hash, FAKE_KEY, &verifier()).unwrap();
        // A version signed by another key than the pinned one is refused.
        let other = read_verified(&lib, "a", 1, &v.manifest_hash, "SHA256:other", &verifier());
        assert!(matches!(&other, Err(SkillError::Modified(m)) if m.contains("another key")), "{other:?}");
        assert_eq!(r.files[0].0, "SKILL.md");
        assert_eq!(r.files[1], ("reference/guide.md".to_string(), b"Guide\n".to_vec()));
        assert_eq!(r.manifest, v.manifest);
        // Private, and no staging folder is left behind.
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&lib), 0o700);
        assert_eq!(mode(&lib.join("a/v1/SKILL.md")), 0o600);
        let names: Vec<String> = std::fs::read_dir(lib.join("a"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into())
            .collect();
        assert_eq!(names, ["v1"]);
        let mut v2 = bundle("a", 2);
        v2.manifest.parent = Some(v.manifest_hash.clone());
        write_version(&lib, &signer(), &v2).unwrap();
        assert_eq!(list_versions(&lib, "a").unwrap(), [1, 2]);
        assert!(list_versions(&lib, "missing").unwrap().is_empty());
    }

    #[test]
    fn a_bundle_that_breaks_the_format_is_refused_before_writing() {
        let (_home, lib) = lib_dir();
        let mut b = bundle("a", 1);
        b.files[0].0 = "README.md".into();
        assert!(write_version(&lib, &signer(), &b).is_err());
        let mut b = bundle("a", 1);
        b.files.push(("run.sh".into(), b"rm -rf".to_vec()));
        assert!(write_version(&lib, &signer(), &b).is_err());
        assert!(write_version(&lib, &signer(), &bundle("../x", 1)).is_err());
        assert!(list_versions(&lib, "a").unwrap().is_empty());
    }

    #[test]
    fn a_changed_extra_or_missing_file_fails_verification() {
        let (_home, lib) = lib_dir();
        let v = write_version(&lib, &signer(), &bundle("a", 1)).unwrap();
        let read = || read_verified(&lib, "a", 1, &v.manifest_hash, FAKE_KEY, &verifier());
        let skill = lib.join("a/v1/SKILL.md");
        let original = std::fs::read(&skill).unwrap();
        std::fs::write(&skill, b"---\nname: a\ndescription: x\n---\nrm -rf ~\n").unwrap();
        assert!(matches!(read(), Err(SkillError::Modified(m)) if m.contains("SKILL.md")), "{:?}", read());
        std::fs::write(&skill, &original).unwrap();
        read().unwrap();
        std::fs::write(lib.join("a/v1/x.md"), "planted").unwrap();
        assert!(matches!(read(), Err(SkillError::Modified(m)) if m.contains("x.md")), "{:?}", read());
        std::fs::remove_file(lib.join("a/v1/x.md")).unwrap();
        std::fs::remove_file(lib.join("a/v1/reference/guide.md")).unwrap();
        assert!(matches!(read(), Err(SkillError::Modified(m)) if m.contains("guide.md")), "{:?}", read());
    }

    #[test]
    fn a_swapped_manifest_or_bad_signature_fails() {
        let (_home, lib) = lib_dir();
        let v = write_version(&lib, &signer(), &bundle("a", 1)).unwrap();
        // The ledger's hash is not this manifest's.
        assert!(matches!(
            read_verified(&lib, "a", 1, &"0".repeat(64), FAKE_KEY, &verifier()),
            Err(SkillError::Modified(_))
        ));
        // A manifest rewritten (a wider scope, say) no longer hashes to the ledger's.
        let m = lib.join("a/v1/mirante-skill.json");
        let text = std::fs::read_to_string(&m).unwrap();
        std::fs::write(
            &m,
            text.replace(r#""kind":"org","value":"github.com/acme""#, r#""kind":"personal","value":"""#),
        )
        .unwrap();
        assert!(read_verified(&lib, "a", 1, &v.manifest_hash, FAKE_KEY, &verifier()).is_err());
        std::fs::write(&m, &text).unwrap();
        // A signature over other bytes.
        std::fs::write(lib.join("a/v1/mirante-skill.sig"), signer().sign(b"other").unwrap()).unwrap();
        assert!(matches!(
            read_verified(&lib, "a", 1, &v.manifest_hash, FAKE_KEY, &verifier()),
            Err(SkillError::Modified(m)) if m.contains("signature")
        ));
    }

    #[test]
    fn symlinked_skill_dir_or_hardlinked_file_is_refused() {
        let (_home, lib) = lib_dir();
        let outside = TempDir::new("skills-out");
        std::fs::create_dir(&lib).unwrap();
        std::os::unix::fs::symlink(outside.path(), lib.join("a")).unwrap();
        assert!(matches!(write_version(&lib, &signer(), &bundle("a", 1)), Err(SkillError::Unsafe(_))));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(read_verified(&lib, "a", 1, &"0".repeat(64), FAKE_KEY, &verifier()).is_err());
        // A version whose SKILL.md is a hard link to a file elsewhere.
        let v = write_version(&lib, &signer(), &bundle("b", 1)).unwrap();
        let skill = lib.join("b/v1/SKILL.md");
        let copy = outside.path().join("copy.md");
        std::fs::copy(&skill, &copy).unwrap();
        std::fs::remove_file(&skill).unwrap();
        std::fs::hard_link(&copy, &skill).unwrap();
        assert!(read_verified(&lib, "b", 1, &v.manifest_hash, FAKE_KEY, &verifier()).is_err());
        // The library itself as a symlink is refused too.
        let (_h2, lib2) = lib_dir();
        std::os::unix::fs::symlink(outside.path(), &lib2).unwrap();
        assert!(write_version(&lib2, &signer(), &bundle("c", 1)).is_err());
    }

    #[test]
    fn remove_version_takes_back_one_version_only() {
        let (_home, lib) = lib_dir();
        write_version(&lib, &signer(), &bundle("a", 1)).unwrap();
        write_version(&lib, &signer(), &bundle("a", 2)).unwrap();
        remove_version(&lib, "a", 2).unwrap();
        assert_eq!(list_versions(&lib, "a").unwrap(), [1]);
        remove_version(&lib, "a", 9).unwrap();
        remove_version(&lib, "missing", 1).unwrap();
        assert!(remove_version(&lib, "../x", 1).is_err());
    }

    #[test]
    fn prune_keeps_the_newest_twenty() {
        let (_home, lib) = lib_dir();
        for v in 1..=KEEP_VERSIONS as u32 + 3 {
            write_version(&lib, &signer(), &bundle("a", v)).unwrap();
        }
        std::fs::create_dir(lib.join("a/.stage-crashed")).unwrap();
        // The current version (the ledger's) is never pruned, however old.
        assert_eq!(prune(&lib, "a", KEEP_VERSIONS, &[2]).unwrap(), [1, 3]);
        let mut left = vec![2];
        left.extend(4..=KEEP_VERSIONS as u32 + 3);
        assert_eq!(list_versions(&lib, "a").unwrap(), left);
        assert!(!lib.join("a/.stage-crashed").exists());
        assert!(prune(&lib, "a", KEEP_VERSIONS, &[2]).unwrap().is_empty());
        assert_eq!(prune(&lib, "a", KEEP_VERSIONS, &[]).unwrap(), [2]);
    }

    #[test]
    fn a_version_folder_with_too_many_entries_is_refused_whatever_their_order() {
        let (_home, lib) = lib_dir();
        let v = write_version(&lib, &signer(), &bundle("a", 1)).unwrap();
        // Empty folders hold no file, but past the listing limit they would hide one.
        for i in 0..70 {
            std::fs::create_dir(lib.join(format!("a/v1/d{i:02}"))).unwrap();
        }
        let r = read_verified(&lib, "a", 1, &v.manifest_hash, FAKE_KEY, &verifier());
        assert!(matches!(&r, Err(SkillError::Modified(m)) if m.contains("too many")), "{r:?}");
    }

    #[test]
    fn round_trips_with_the_real_ssh_keygen() {
        if !Path::new(DEFAULT_SSH_KEYGEN).exists() {
            eprintln!("skipped: {DEFAULT_SSH_KEYGEN} is not installed");
            return;
        }
        let (home, lib) = lib_dir();
        let k = SshKeygen::ensure_key_with(home.path(), Path::new(DEFAULT_SSH_KEYGEN)).unwrap();
        let v = write_version(&lib, &k, &bundle("a", 1)).unwrap();
        assert!(v.manifest.signer.key.starts_with("SHA256:"));
        read_verified(&lib, "a", 1, &v.manifest_hash, &v.manifest.signer.key, &k).unwrap();
        std::fs::write(lib.join("a/v1/mirante-skill.sig"), k.sign(b"other\n").unwrap()).unwrap();
        assert!(read_verified(&lib, "a", 1, &v.manifest_hash, &v.manifest.signer.key, &k).is_err());
    }
}
