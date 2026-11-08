//! `mirante skills lint <SKILL.md>`: the daemon's lint, run offline on a skill folder (the
//! `SKILL.md` and the text files beside it), with no daemon and nothing written.
use std::path::Path;

use mirante_core::dna::untrusted::line;
use mirante_core::skills::format::parse_skill_md;
use mirante_core::skills::lint::{lint, Draft, Finding, Severity};
use mirante_core::skills::{MANIFEST, MAX_FILE, SIGNATURE, SKILL_MD};

/// Largest `SKILL.md` read.
const MAX_SKILL_MD: u64 = 1 << 20;
/// Most files read beside it (lint reports more than 8 as an error itself).
const MAX_ENTRIES: usize = 64;
/// How deep bundled files are looked for (lint allows 3 levels).
const MAX_DEPTH: usize = 3;

fn read_capped(p: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut out = Vec::new();
    f.take(max + 1).read_to_end(&mut out).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(out)
}

/// Regular files (never through a symlink) under `dir`, as `a/b.md` paths, skipping
/// `SKILL.md`, a version folder's manifest and signature, and hidden names.
fn bundled(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rel = format!("{prefix}{name}");
        if name.starts_with('.') || (prefix.is_empty() && [SKILL_MD, MANIFEST, SIGNATURE].contains(&name.as_str())) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if meta.is_dir() && depth < MAX_DEPTH {
            bundled(&p, &format!("{rel}/"), depth + 1, out)?;
        } else if meta.is_file() {
            out.push((rel, read_capped(&p, MAX_FILE as u64)?));
        }
    }
    Ok(())
}

/// Lints the skill whose `SKILL.md` is `file`. `Err` when it could not run (the file
/// cannot be read); a `SKILL.md` that does not parse is a `format` error finding. Line
/// numbers of `SKILL.md` findings are the file's (frontmatter counted).
pub fn lint_file(file: &Path) -> Result<Vec<Finding>, String> {
    let bytes = read_capped(file, MAX_SKILL_MD)?;
    let text = String::from_utf8_lossy(&bytes);
    let parsed = match parse_skill_md(&text) {
        Ok(p) => p,
        Err(e) => {
            let message = e.to_string();
            return Ok(vec![Finding {
                rule: "format".into(),
                severity: Severity::Error,
                file: SKILL_MD.into(),
                line: 1,
                message,
            }]);
        }
    };
    let mut files = Vec::new();
    if let Some(dir) = file.parent().map(|d| if d.as_os_str().is_empty() { Path::new(".") } else { d }) {
        bundled(dir, "", 1, &mut files)?;
    }
    let offset = text[..text.len() - parsed.body.len()].lines().count() as u32;
    let draft = Draft {
        name: parsed.name,
        description: parsed.description,
        body: parsed.body,
        files,
        extra_keys: parsed.extra_keys,
    };
    let mut findings = lint(&draft);
    for f in &mut findings {
        if f.file == SKILL_MD && f.line > 0 {
            f.line += offset;
        }
    }
    Ok(findings)
}

/// `rule (severity) file:line: message`, every field cleaned for the terminal.
pub fn finding_line(f: &Finding) -> String {
    let severity = match f.severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
    };
    format!("{} ({severity}) {}:{}: {}", line(&f.rule, 40), line(&f.file, 200), f.line, line(&f.message, 300))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    #[test]
    fn findings_carry_the_files_line_and_bundled_files_count() {
        let d = TestDir::new();
        std::fs::create_dir_all(d.path().join("reference")).unwrap();
        std::fs::write(d.path().join("reference/a.md"), "ref\n").unwrap();
        let md = d.path().join("SKILL.md");
        std::fs::write(&md, "---\nname: a\ndescription: d\n---\nSee [a](./reference/a.md).\nsudo rm x\n").unwrap();
        let f = lint_file(&md).unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!((f[0].rule.as_str(), f[0].line), ("shell", 6));
        assert_eq!(finding_line(&f[0]), format!("shell (error) SKILL.md:6: {}", f[0].message));
        std::fs::remove_file(d.path().join("reference/a.md")).unwrap();
        assert!(lint_file(&md).unwrap().iter().any(|f| f.rule == "refs"));
        std::fs::write(&md, "no frontmatter").unwrap();
        assert_eq!(lint_file(&md).unwrap()[0].rule, "format");
        assert!(lint_file(&d.path().join("missing.md")).is_err());
    }
}
