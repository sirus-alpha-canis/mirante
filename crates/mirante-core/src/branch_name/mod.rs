//! Branch names made from a goal, for a task created without one: `mirante/<slug>`, and
//! `-2`, `-3`… past the names already taken. The cockpit's `branchFromGoal` /
//! `nextFreeBranch` (apps/cockpit/src/lib/dispatch.ts) follow the same rules; both test
//! suites check `branch_name/vectors.json`.
use crate::dna::note::fold;

/// Where auto-named branches live.
pub const BRANCH_PREFIX: &str = "mirante/";

/// The slug is cut near this many bytes (ASCII), at a word boundary when there is one.
const SLUG_MAX: usize = 40;

/// Combining diacritical marks (a goal typed in decomposed form): dropped.
fn combining(c: char) -> bool {
    ('\u{300}'..='\u{36f}').contains(&c)
}

/// "Leia este repositório e me explique" -> "mirante/leia-este-repositorio-e-me-explique":
/// lowercase ASCII, common accents folded, every other run of characters one `-`, cut near
/// 40 characters at a word boundary; "mirante/task" when nothing is left.
pub fn branch_from_goal(goal: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in goal.chars().flat_map(char::to_lowercase) {
        if combining(c) {
            continue;
        }
        let ascii;
        let piece = match c {
            'a'..='z' | '0'..='9' => {
                ascii = c.to_string();
                Some(ascii.as_str())
            }
            c => fold(c),
        };
        match piece {
            Some(p) => {
                if dash && !slug.is_empty() {
                    slug.push('-');
                }
                dash = false;
                slug.push_str(p);
            }
            None => dash = true,
        }
    }
    if slug.len() > SLUG_MAX {
        match slug[..=SLUG_MAX].rfind('-') {
            Some(cut) if cut > 0 => slug.truncate(cut),
            _ => slug.truncate(SLUG_MAX),
        }
    }
    if slug.is_empty() {
        slug.push_str("task");
    }
    format!("{BRANCH_PREFIX}{slug}")
}

/// `base`, or `base-2`, `base-3`… the first not in `taken`.
pub fn next_free_branch(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|b| !taken.contains(b)).expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cockpit's dispatch.test.ts checks the same file.
    #[test]
    fn branch_names_match_the_shared_vectors() {
        let v: serde_json::Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
        for case in v["branch_from_goal"].as_array().unwrap() {
            let (goal, want) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
            assert_eq!(branch_from_goal(goal), want, "{goal:?}");
        }
        for case in v["next_free"].as_array().unwrap() {
            let base = case[0].as_str().unwrap();
            let taken: Vec<String> = case[1].as_array().unwrap().iter().map(|t| t.as_str().unwrap().into()).collect();
            assert_eq!(next_free_branch(base, &taken), case[2].as_str().unwrap(), "{base} {taken:?}");
        }
    }

    #[test]
    fn every_name_is_a_valid_git_branch() {
        for g in ["..lock", "-rf /", "x.lock", "@{u}", "a//b", "ÀÉÎõü", "a.", "/", "~^:?*[\\"] {
            let b = branch_from_goal(g);
            assert!(crate::worktree::validate_branch_name(&b).is_ok(), "{g:?} -> {b}");
        }
    }
}
