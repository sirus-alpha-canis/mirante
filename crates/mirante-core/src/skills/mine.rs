//! The miner: instructions the operator kept repeating, found without a model.
//!
//! Input is Mirante's own history of one tenant (task goals, steers, queued messages and
//! denied permissions; allowed permissions are never mined, so nothing pushes toward
//! auto-approval). Each text is redacted, split into units (lines, `. `, `; `, `? `,
//! bullets stripped, 4 to 60 tokens kept), tokenized (lowercase, accents folded, split on
//! non-alphanumerics, a small English and Portuguese stopword list) and clustered greedily
//! in time order: a unit joins a cluster of its tenant when its token set has Jaccard ≥ 0.5
//! or containment ≥ 0.8 with the cluster's first unit. Denials cluster on (tool, the first
//! two words of the command, kept to `[A-Za-z0-9._/-]`: the command is the agent's). A cluster qualifies with at least 3 distinct tasks; tenants
//! never mix. [`draft`] turns one into a proposal: skipped when an active in-scope skill
//! already says it, an edit of a repo or org skill of the same tenant that was active in
//! ≥ 2/3 of its tasks and is about the same thing (≥ 1/3 of the line's words, stopwords
//! aside, in its name, description or body: [`relevance`]); never a personal skill: that
//! would carry one client's words to every other), else a new always-on skill. Deterministic: the same history gives the same drafts.
use std::collections::{HashMap, HashSet};

use super::format::{render_skill_md, Activation};
use super::lint::Draft;
use super::origin::Scope;
use crate::dna::note::{fold, slugify};
use crate::dna::sha256_hex;
use crate::dna::untrusted;
use crate::redact::redact;

/// Mirante's own message templates start with this; they are not the operator's words.
pub const TEMPLATE_PREFIX: &str = "Operator request:";

const STOPWORDS: &[&str] = &[
    // English
    "a", "an", "the", "and", "or", "but", "if", "then", "to", "of", "in", "on", "at", "for", "with", "by", "from", "as",
    "is", "are", "was", "were", "be", "been", "it", "its", "this", "that", "these", "those", "you", "we", "they", "he",
    "she", "me", "my", "your", "our", "please", "pls", "ok", "okay", "just", "also", "do", "does", "did", "can",
    "could", "would", "should", "will", "so", "too", "very", "there", "here", "what", "when", "where", "which", "who",
    "how", "all", "any", "some", "each", "thanks", "thank", "now", "again", // Portuguese (accents folded)
    "o", "os", "as", "um", "uma", "uns", "umas", "de", "do", "da", "dos", "das", "no", "na", "nos", "nas", "em", "por",
    "para", "pra", "com", "que", "e", "ou", "mas", "se", "ao", "aos", "eh", "sao", "ser", "foi", "isso", "isto",
    "esse", "essa", "este", "esta", "eu", "voce", "vc", "ele", "ela", "eles", "elas", "meu", "minha", "seu", "sua",
    "favor", "obrigado", "obrigada", "tambem", "ja", "so", "muito", "mais", "como", "quando", "onde", "qual", "agora",
    "vai", "ai",
];

/// Where an utterance came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Goal,
    Steer,
    Queue,
    /// A permission request the operator denied; `text` is its redacted detail.
    Denied {
        tool: String,
    },
}

/// One thing the operator said (or denied) in a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub task: String,
    pub repo_key: String,
    pub tenant: String,
    pub text: String,
    /// Milliseconds since the epoch.
    pub ts: i64,
    pub source: Source,
}

/// One redacted sentence of an utterance, with its tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub task: String,
    pub repo_key: String,
    pub tenant: String,
    pub text: String,
    pub tokens: Vec<String>,
    pub ts: i64,
    /// For a denial: (tool, the first two words of the command).
    pub denial: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub jaccard: f64,
    pub containment: f64,
    pub min_tasks: usize,
    pub min_tokens: usize,
    pub max_tokens: usize,
    /// Most units clustered per tenant (the newest are kept).
    pub max_units: usize,
    /// Units older than this (ms since the epoch) are ignored.
    pub since_ms: i64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            jaccard: 0.5,
            containment: 0.8,
            min_tasks: 3,
            min_tokens: 4,
            max_tokens: 60,
            max_units: 5000,
            since_ms: 0,
        }
    }
}

/// Lowercased, accent-folded alphanumeric words of `text`, without stopwords or one-letter
/// words, each once, in order.
pub fn tokens(text: &str) -> Vec<String> {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match fold(c) {
            Some(s) => folded.push_str(s),
            None if c.is_alphanumeric() => folded.push(c),
            None => folded.push(' '),
        }
    }
    let mut seen = HashSet::new();
    folded
        .split_whitespace()
        .filter(|w| w.chars().count() > 1 && !STOPWORDS.contains(w))
        .filter(|w| seen.insert(w.to_string()))
        .map(str::to_string)
        .collect()
}

/// `text` without a leading bullet (`-`, `*`, `•`, `+`, `1.`, `1)`).
fn strip_bullet(s: &str) -> &str {
    let t = s.trim_start();
    for b in ["- ", "* ", "• ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            return rest.trim_start();
        }
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && digits <= 3 {
        if let Some(rest) = t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") ")) {
            return rest.trim_start();
        }
    }
    t
}

/// The sentences of `text`: split on lines, `. `, `; ` and `? `, bullets stripped, trailing
/// punctuation trimmed.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = vec![line.to_string()];
        for sep in [". ", "; ", "? "] {
            parts = parts.iter().flat_map(|p| p.split(sep).map(str::to_string).collect::<Vec<_>>()).collect();
        }
        for p in parts {
            let s = strip_bullet(&p).trim().trim_end_matches(['.', '!', '?', ';', ',', ':']).trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// The units of `utterances`: Mirante's templates skipped, every text redacted before it is
/// split or tokenized, units outside 4..=60 tokens dropped (denials are kept whole).
pub fn units(utterances: &[Utterance]) -> Vec<Unit> {
    let p = Params::default();
    let mut out = Vec::new();
    for u in utterances {
        if u.text.trim_start().starts_with(TEMPLATE_PREFIX) {
            continue;
        }
        let unit = |text: String, tokens: Vec<String>, denial| Unit {
            task: u.task.clone(),
            repo_key: u.repo_key.clone(),
            tenant: u.tenant.clone(),
            text,
            tokens,
            ts: u.ts,
            denial,
        };
        match &u.source {
            Source::Denied { tool } => {
                let clean = untrusted::line(&redact(&u.text), 4096);
                // The command is the agent's: only plain command characters reach the draft.
                let head: Vec<String> = clean
                    .split_whitespace()
                    .map(|w| {
                        w.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')).collect()
                    })
                    .filter(|w: &String| !w.is_empty())
                    .take(2)
                    .collect();
                if head.is_empty() {
                    continue;
                }
                let head = head.join(" ");
                out.push(unit(clean.clone(), tokens(&clean), Some((tool.clone(), head))));
            }
            _ => {
                for s in sentences(&redact(&u.text)) {
                    let s = untrusted::line(&s, 1024);
                    let t = tokens(&s);
                    if (p.min_tokens..=p.max_tokens).contains(&t.len()) {
                        out.push(unit(s, t, None));
                    }
                }
            }
        }
    }
    out
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let (a, b): (HashSet<&String>, HashSet<&String>) = (a.iter().collect(), b.iter().collect());
    let inter = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn containment(a: &[String], b: &[String]) -> f64 {
    let (a, b): (HashSet<&String>, HashSet<&String>) = (a.iter().collect(), b.iter().collect());
    let min = a.len().min(b.len()) as f64;
    if min == 0.0 {
        0.0
    } else {
        a.intersection(&b).count() as f64 / min
    }
}

/// Units of one tenant that say the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub tenant: String,
    pub units: Vec<Unit>,
    /// Distinct tasks, in order of first appearance.
    pub tasks: Vec<String>,
    /// Distinct repo keys, in order of first appearance.
    pub repos: Vec<String>,
    pub denial: Option<(String, String)>,
}

/// The qualifying clusters of `units` (see the module docs), oldest first. The newest
/// `max_units` are kept per tenant (the hub mines one tenant at a time; a mixed input never
/// lets one tenant's volume push another's evidence out).
pub fn cluster(units: &[Unit], p: &Params) -> Vec<Cluster> {
    let mut sorted: Vec<&Unit> = units.iter().filter(|u| u.ts >= p.since_ms).collect();
    sorted.sort_by_key(|u| u.ts); // stable: same-time units keep their order
    let mut per_tenant: HashMap<&str, usize> = HashMap::new();
    let mut keep = vec![false; sorted.len()];
    for (i, u) in sorted.iter().enumerate().rev() {
        let n = per_tenant.entry(u.tenant.as_str()).or_default();
        if *n < p.max_units {
            *n += 1;
            keep[i] = true;
        }
    }
    let mut kept = keep.into_iter();
    sorted.retain(|_| kept.next().unwrap_or(false));
    let mut clusters: Vec<Cluster> = Vec::new();
    for u in sorted {
        let joins = |c: &Cluster| {
            c.tenant == u.tenant
                && match (&c.denial, &u.denial) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => {
                        let seed = &c.units[0].tokens;
                        jaccard(seed, &u.tokens) >= p.jaccard || containment(seed, &u.tokens) >= p.containment
                    }
                    _ => false,
                }
        };
        match clusters.iter_mut().find(|c| joins(c)) {
            Some(c) => {
                if !c.tasks.contains(&u.task) {
                    c.tasks.push(u.task.clone());
                }
                if !c.repos.contains(&u.repo_key) {
                    c.repos.push(u.repo_key.clone());
                }
                c.units.push(u.clone());
            }
            None => clusters.push(Cluster {
                tenant: u.tenant.clone(),
                units: vec![u.clone()],
                tasks: vec![u.task.clone()],
                repos: vec![u.repo_key.clone()],
                denial: u.denial.clone(),
            }),
        }
    }
    clusters.retain(|c| c.tasks.len() >= p.min_tasks);
    clusters
}

/// `sha256(tenant + sorted tokens)[..16]`: the same instruction in another order or
/// spelling (accents) has the same fingerprint; another tenant's never does.
pub fn fingerprint(tenant: &str, tokens: &[String]) -> String {
    let mut t: Vec<&str> = tokens.iter().map(String::as_str).collect();
    t.sort_unstable();
    t.dedup();
    sha256_hex(format!("{tenant}\n{}", t.join(" ")).as_bytes())[..16].to_string()
}

/// A skill that is active, as the miner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSkill {
    pub name: String,
    pub manifest_hash: String,
    pub scope: Scope,
    pub description: String,
    pub body: String,
    /// An edit keeps it.
    pub activation: Activation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    New,
    /// Append to `name`, whose current manifest hash is `base_hash`.
    Edit {
        name: String,
        base_hash: String,
    },
}

/// One example of the evidence: a task and what was said there (redacted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub task: String,
    pub text: String,
}

/// A proposal the miner drafts (a person reviews it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mined {
    pub kind: Kind,
    pub tenant: String,
    pub name: String,
    pub description: String,
    pub body: String,
    pub activation: Activation,
    pub scope: Scope,
    pub fingerprint: String,
    /// Distinct tasks in the cluster.
    pub tasks: usize,
    /// At most 5, one per task.
    pub evidence: Vec<Evidence>,
}

impl Mined {
    /// What [`crate::skills::lint::lint`] reads for this draft.
    pub fn lint_draft(&self) -> Draft {
        Draft {
            name: self.name.clone(),
            description: self.description.clone(),
            body: self.body.clone(),
            files: Vec::new(),
            extra_keys: Vec::new(),
        }
    }

    /// The `SKILL.md` this draft would write.
    pub fn skill_md(&self) -> String {
        render_skill_md(&self.name, &self.description, &self.body)
    }
}

/// The unit most similar to all the others (the first on a tie).
fn medoid(c: &Cluster) -> &Unit {
    let mut best = (&c.units[0], f64::MIN);
    for u in &c.units {
        let score: f64 = c.units.iter().map(|v| jaccard(&u.tokens, &v.tokens)).sum();
        if score > best.1 {
            best = (u, score);
        }
    }
    best.0
}

/// A name from the cluster's three most frequent tokens (ties: the medoid's word order),
/// without `claude`/`anthropic`, `-2`, `-3`… when taken.
fn new_name(c: &Cluster, medoid: &Unit, taken: &[String]) -> String {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for u in &c.units {
        for t in &u.tokens {
            *count.entry(t).or_default() += 1;
        }
    }
    let pos = |t: &str| medoid.tokens.iter().position(|m| m == t).unwrap_or(usize::MAX);
    let mut ranked: Vec<&str> =
        count.keys().copied().filter(|t| !t.contains("claude") && !t.contains("anthropic")).collect();
    ranked.sort_by(|a, b| count[b].cmp(&count[a]).then(pos(a).cmp(&pos(b))).then(a.cmp(b)));
    let base = match &c.denial {
        Some((_, head)) => slugify(&format!("ask before {head}")),
        None if ranked.is_empty() => "repeated-instruction".to_string(),
        None => slugify(&ranked.iter().take(3).copied().collect::<Vec<_>>().join(" ")),
    };
    let base = if crate::skills::format::is_name(&base) { base } else { "repeated-instruction".to_string() };
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
        let suffix = format!("-{n}");
        let mut b = base.clone();
        b.truncate(64 - suffix.len());
        name = format!("{}{suffix}", b.trim_end_matches('-'));
        n += 1;
    }
    name
}

/// One line of text safe for a description: no `<`, `>` or line breaks.
fn description_text(s: &str) -> String {
    untrusted::line(s, 120).replace(['<', '>'], " ")
}

/// Least share of the mined line's words an existing skill must also say (in its name,
/// description or body) before the line is proposed as an edit of it: a third.
pub const MIN_RELEVANCE: f64 = 1.0 / 3.0;

/// Two words name the same thing: equal; or one starts the other and the shorter, the
/// stem, has at least five letters (`commit` / `commitar`); or a stem of four letters
/// with at most two more (`test` / `tests`, never `test` / `testament`): a crude stem
/// across PT and EN.
fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.starts_with(short) && (short.len() >= 5 || (short.len() == 4 && long.len() - short.len() <= 2))
}

/// The share of `line` (its tokens) that `skill` also says, in its name, description or
/// body (stopwords never count: [`tokens`] drops them). Deterministic, no model.
pub fn relevance(line: &[String], skill: &ActiveSkill) -> f64 {
    if line.is_empty() {
        return 0.0;
    }
    let said = tokens(&format!("{}\n{}\n{}", skill.name, skill.description, skill.body));
    let shared = line.iter().filter(|w| said.iter().any(|s| same_word(w, s))).count();
    shared as f64 / line.len() as f64
}

/// The proposal for cluster `c`, or `None` when it is covered: an active skill in scope of
/// the cluster's repos has a body line with Jaccard ≥ 0.5 to the representative (the
/// medoid). `active`: active skills; `active_in_tasks`: the skills active in each task;
/// `taken`: skill names in use (a new name gets `-2`…).
pub fn draft(
    c: &Cluster,
    active: &[ActiveSkill],
    active_in_tasks: &HashMap<String, Vec<String>>,
    taken: &[String],
) -> Option<Mined> {
    let rep = medoid(c);
    let in_scope: Vec<&ActiveSkill> = active.iter().filter(|s| c.repos.iter().any(|r| s.scope.matches(r))).collect();
    let line = match &c.denial {
        Some((_, head)) => {
            format!("Do not run `{head}`; ask the operator first (denied in {} tasks).", c.tasks.len())
        }
        None => rep.text.clone(),
    };
    let covered = in_scope.iter().any(|s| {
        s.body.lines().any(|l| {
            let t = tokens(l);
            !t.is_empty() && jaccard(&t, &tokens(&line)) >= 0.5
        })
    });
    if covered {
        return None;
    }
    let fp_tokens = match &c.denial {
        Some((tool, head)) => {
            let mut t = vec!["denied".to_string(), tool.to_lowercase()];
            t.extend(tokens(head));
            t
        }
        None => rep.tokens.clone(),
    };
    let fingerprint = fingerprint(&c.tenant, &fp_tokens);
    let mut evidence = Vec::new();
    for task in &c.tasks {
        if evidence.len() == 5 {
            break;
        }
        if let Some(u) = c.units.iter().find(|u| &u.task == task) {
            evidence.push(Evidence { task: task.clone(), text: u.text.clone() });
        }
    }
    let scope = match &c.repos[..] {
        [one] => Scope::Repo(one.clone()),
        _ => Scope::Org(c.tenant.clone()),
    };
    // An edit when one in-scope skill of the cluster's own tenant (a repo or org scope
    // inside it) was active in at least 2/3 of the cluster's tasks and is about the same
    // thing (at least [`MIN_RELEVANCE`] of the line's words in its name, description or
    // body): a skill that merely ran alongside (an API-lookup skill in tasks where the
    // operator kept asking for clippy) is not where the line belongs. A personal skill (or
    // one of another tenant) may cover a line but is never edited: the tenant's words would
    // reach every other client through it.
    let own_tenant = |s: &ActiveSkill| match &s.scope {
        Scope::Repo(k) | Scope::Org(k) => super::origin::tenant(k) == c.tenant,
        Scope::Personal => false,
    };
    // What the line is about: a denial's command (not its tool: every shell rule names
    // bash), else the line's own words.
    let topic = match &c.denial {
        Some((_, head)) => tokens(head),
        None => tokens(&line),
    };
    let ran_with = |s: &ActiveSkill| {
        c.tasks.iter().filter(|t| active_in_tasks.get(*t).is_some_and(|names| names.contains(&s.name))).count()
    };
    let mut candidates: Vec<(&ActiveSkill, usize)> = in_scope
        .iter()
        .filter(|s| own_tenant(s) && relevance(&topic, s) >= MIN_RELEVANCE)
        .map(|s| (*s, ran_with(s)))
        .filter(|(_, n)| n * 3 >= c.tasks.len() * 2)
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.name.cmp(&b.0.name)));
    if let Some((s, _)) = candidates.first() {
        let mut body = s.body.clone();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&line);
        body.push('\n');
        return Some(Mined {
            kind: Kind::Edit { name: s.name.clone(), base_hash: s.manifest_hash.clone() },
            tenant: c.tenant.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            body,
            activation: s.activation,
            scope: s.scope.clone(),
            fingerprint,
            tasks: c.tasks.len(),
            evidence,
        });
    }
    Some(Mined {
        kind: Kind::New,
        tenant: c.tenant.clone(),
        name: new_name(c, rep, taken),
        description: format!(
            "Instructions the operator repeated in {} tasks: {}",
            c.tasks.len(),
            description_text(&line)
        ),
        body: format!("{line}\n"),
        activation: Activation::Always,
        scope,
        fingerprint,
        tasks: c.tasks.len(),
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::lint::{has_errors, lint};

    const A: &str = "github.com/acme";
    const B: &str = "github.com/other";

    fn utt_in(tenant: &str, repo: &str, task: &str, text: &str, ts: i64) -> Utterance {
        Utterance {
            task: task.into(),
            repo_key: format!("{tenant}/{repo}"),
            tenant: tenant.into(),
            text: text.into(),
            ts,
            source: Source::Steer,
        }
    }

    fn utt(task: &str, text: &str) -> Utterance {
        let ts = task.trim_start_matches('t').parse::<i64>().unwrap_or(0) * 1000;
        utt_in(A, "api", task, text, ts)
    }

    fn clippy_three() -> Vec<Utterance> {
        vec![
            utt("t1", "Rode cargo clippy antes de commitar, por favor."),
            utt("t2", "antes de commitar rode o cargo clippy"),
            utt("t3", "Please, rode cargo clippy antes de commitar!"),
            utt("t3", "fix the login bug"),
        ]
    }

    #[test]
    fn three_tasks_repeating_an_instruction_make_one_cluster() {
        let c = cluster(&units(&clippy_three()), &Params::default());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].tasks.len(), 3);
        assert_eq!(c[0].tenant, A);
    }

    #[test]
    fn two_tasks_are_not_enough_and_tenants_never_mix() {
        let u = vec![
            utt_in(A, "api", "t1", "Rode cargo clippy antes de commitar", 1),
            utt_in(A, "web", "t2", "rode o cargo clippy antes de commitar", 2),
            utt_in(B, "api", "t3", "rode cargo clippy antes de commitar", 3),
        ];
        assert!(cluster(&units(&u), &Params::default()).is_empty());
        // The same instruction in a third task of A qualifies, B's never joins it.
        let mut u = u;
        u.push(utt_in(A, "api", "t4", "rode cargo clippy antes de commitar sempre", 4));
        let c = cluster(&units(&u), &Params::default());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].tasks, ["t1", "t2", "t4"]);
        // Old evidence outside the window does not count.
        let p = Params { since_ms: 2, ..Params::default() };
        assert!(cluster(&units(&u), &p).is_empty());
    }

    #[test]
    fn mirante_templates_and_short_units_are_skipped() {
        let mut u = Vec::new();
        for t in ["t1", "t2", "t3"] {
            u.push(utt(t, "Operator request: refine the skill cargo clippy antes de commitar"));
            u.push(utt(t, "ok go"));
        }
        assert!(units(&u).is_empty(), "{:?}", units(&u));
        assert!(cluster(&units(&u), &Params::default()).is_empty());
        // Bullets and sentence splits: one message holds several units.
        let parts =
            units(&[utt("t1", "- Rode cargo clippy antes de commitar. Depois rode os testes do core inteiro; ok")]);
        let texts: Vec<&str> = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["Rode cargo clippy antes de commitar", "Depois rode os testes do core inteiro"]);
    }

    #[test]
    fn units_are_redacted_before_tokenizing() {
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let u: Vec<Utterance> = ["t1", "t2", "t3"]
            .iter()
            .map(|t| utt(t, &format!("use TOKEN={token} to push the release branch")))
            .collect();
        let us = units(&u);
        assert!(us.iter().all(|x| !x.text.contains("ghp_") && x.text.contains("«redacted»")), "{us:?}");
        assert!(us.iter().all(|x| x.tokens.iter().all(|t| !t.contains("ghp"))));
        let c = cluster(&us, &Params::default());
        let d = draft(&c[0], &[], &HashMap::new(), &[]).unwrap();
        assert!(d.body.contains("«redacted»") && !d.body.contains("ghp_"), "{}", d.body);
        assert!(d.evidence.iter().all(|e| !e.text.contains("ghp_")));
    }

    fn active(name: &str, scope: Scope, body: &str) -> ActiveSkill {
        ActiveSkill {
            name: name.into(),
            manifest_hash: "h".repeat(64),
            scope,
            description: format!("{name} rules"),
            body: body.into(),
            activation: Activation::OnDemand,
        }
    }

    #[test]
    fn covered_by_an_active_skill_is_skipped_else_edit_when_that_skill_was_active() {
        let c = cluster(&units(&clippy_three()), &Params::default());
        // Covered: an in-scope skill already says it.
        let covering = active("lint-first", Scope::org(A), "Always:\nrode cargo clippy antes de commitar\n");
        assert!(draft(&c[0], std::slice::from_ref(&covering), &HashMap::new(), &[]).is_none());
        // The same skill out of scope does not cover it.
        let elsewhere = active("lint-first", Scope::org(B), "rode cargo clippy antes de commitar\n");
        assert!(draft(&c[0], &[elsewhere], &HashMap::new(), &[]).is_some());
        // Not covered, but an in-scope skill was active in 2 of the 3 tasks: an edit of it.
        // (Related: its description shares the line's words, see `relevance`.)
        let style = ActiveSkill {
            description: "House style for cargo projects: what to run before a commit".into(),
            ..active("house-style", Scope::org(A), "Use tabs.\n")
        };
        let ran: HashMap<String, Vec<String>> = [("t1", vec!["house-style"]), ("t3", vec!["house-style"])]
            .iter()
            .map(|(t, v)| (t.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect();
        let d = draft(&c[0], std::slice::from_ref(&style), &ran, &[]).unwrap();
        assert_eq!(d.kind, Kind::Edit { name: "house-style".into(), base_hash: "h".repeat(64) });
        assert_eq!(d.name, "house-style");
        assert!(d.body.starts_with("Use tabs.\n") && d.body.lines().count() == 2, "{}", d.body);
        // An edit keeps the skill's activation: an on-demand skill never becomes always on
        // (the system prompt of every session) because one line was added to it.
        assert_eq!(d.activation, Activation::OnDemand);
        // Active in only 1 of 3: a new skill.
        let once: HashMap<String, Vec<String>> = [("t1".to_string(), vec!["house-style".to_string()])].into();
        assert_eq!(draft(&c[0], &[style], &once, &[]).unwrap().kind, Kind::New);
    }

    /// The dogfood case: the clippy rule was appended to an API-lookup skill only because
    /// that skill ran in 2 of the 3 tasks. Unrelated: a new skill instead.
    #[test]
    fn an_unrelated_skill_that_ran_in_the_tasks_is_not_edited() {
        let c = cluster(&units(&clippy_three()), &Params::default());
        let api = ActiveSkill {
            description: "Look up an HTTP endpoint in the OpenAPI spec before calling it".into(),
            ..active("api-lookup", Scope::org(A), "Find the route in openapi.yaml and read its schema.\n")
        };
        let ran: HashMap<String, Vec<String>> = [("t1", vec!["api-lookup"]), ("t3", vec!["api-lookup"])]
            .iter()
            .map(|(t, v)| (t.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect();
        let d = draft(&c[0], std::slice::from_ref(&api), &ran, &[]).unwrap();
        assert_eq!(d.kind, Kind::New, "the clippy rule has nothing to do with API lookups");
        assert!(d.body.contains("clippy") && !d.body.contains("openapi"), "{}", d.body);
    }

    /// A skill about the same thing (its name, description or body share the line's words)
    /// that ran in the tasks is edited.
    #[test]
    fn a_related_skill_that_ran_in_the_tasks_is_edited() {
        let c = cluster(&units(&clippy_three()), &Params::default());
        let rust = ActiveSkill {
            description: "Rust checks before a commit".into(),
            ..active("rust-checks", Scope::org(A), "Run cargo fmt before every commit.\n")
        };
        let ran: HashMap<String, Vec<String>> = [("t1", vec!["rust-checks"]), ("t2", vec!["rust-checks"])]
            .iter()
            .map(|(t, v)| (t.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect();
        let d = draft(&c[0], std::slice::from_ref(&rust), &ran, &[]).unwrap();
        assert_eq!(d.kind, Kind::Edit { name: "rust-checks".into(), base_hash: "h".repeat(64) });
    }

    #[test]
    fn a_personal_or_other_tenant_skill_is_never_edited_with_a_tenants_words() {
        // Three tasks of tenant A over two repos, an always-on personal skill active in all.
        let mut u = clippy_three();
        u[1].repo_key = format!("{A}/web");
        let c = cluster(&units(&u), &Params::default());
        let all: HashMap<String, Vec<String>> =
            ["t1", "t2", "t3"].iter().map(|t| (t.to_string(), vec!["mine".to_string()])).collect();
        let personal = active("mine", Scope::Personal, "Be terse. Run cargo clippy checks before every commit.\n");
        let d = draft(&c[0], std::slice::from_ref(&personal), &all, &[]).unwrap();
        assert_eq!(d.kind, Kind::New, "A's line would leak into every client through a personal skill");
        assert_eq!(d.scope, Scope::org(A));
        // A personal skill that already says it still covers it.
        let says = active("mine", Scope::Personal, "rode cargo clippy antes de commitar\n");
        assert!(draft(&c[0], &[says], &all, &[]).is_none());
        // A repo or org skill of the tenant itself is an edit candidate.
        let org = active("mine", Scope::org(A), "Be terse. Run cargo clippy checks before every commit.\n");
        assert!(matches!(draft(&c[0], &[org], &all, &[]).unwrap().kind, Kind::Edit { .. }));
        let repo = active(
            "mine",
            Scope::repo(&format!("{A}/api")),
            "Be terse. Run cargo clippy checks before every commit.\n",
        );
        assert!(matches!(draft(&c[0], &[repo], &all, &[]).unwrap().kind, Kind::Edit { .. }));
    }

    #[test]
    fn denials_cluster_on_tool_and_command_head() {
        let denied = |task: &str, cmd: &str, ts: i64| Utterance {
            source: Source::Denied { tool: "Bash".into() },
            ..utt_in(A, "api", task, cmd, ts)
        };
        let u = vec![
            denied("t1", "git push origin feature/x", 1),
            denied("t2", "git push --force origin main", 2),
            denied("t3", "git push", 3),
            denied("t3", "git status", 4),
        ];
        let c = cluster(&units(&u), &Params::default());
        assert_eq!(c.len(), 1);
        let d = draft(&c[0], &[], &HashMap::new(), &[]).unwrap();
        assert_eq!(d.body.trim_end(), "Do not run `git push`; ask the operator first (denied in 3 tasks).");
        assert_eq!(d.kind, Kind::New);
        assert!(!has_errors(&lint(&d.lint_draft())), "{:?}", lint(&d.lint_draft()));
    }

    /// Relevance: the denial's tool name says nothing about the command (every shell rule
    /// mentions bash), and a short stem is not a word (`test` is not `testament`).
    #[test]
    fn relevance_ignores_the_tool_and_needs_a_real_stem() {
        assert!(same_word("test", "tests") && same_word("commit", "commitar") && same_word("cargo", "cargo"));
        assert!(!same_word("test", "testament") && !same_word("run", "running") && !same_word("push", "pushover"));
        let denied = |task: &str, ts: i64| Utterance {
            source: Source::Denied { tool: "Bash".into() },
            ..utt_in(A, "api", task, "git push origin main", ts)
        };
        let c = cluster(&units(&[denied("t1", 1), denied("t2", 2), denied("t3", 3)]), &Params::default());
        let all: HashMap<String, Vec<String>> =
            ["t1", "t2", "t3"].iter().map(|t| (t.to_string(), vec!["shell".to_string()])).collect();
        // Says "bash" but nothing of the command: the tool name does not make it related.
        let shell = ActiveSkill {
            description: "Bash habits".into(),
            ..active("shell", Scope::org(A), "Prefer bash over sh. Quote every variable.\n")
        };
        assert_eq!(draft(&c[0], std::slice::from_ref(&shell), &all, &[]).unwrap().kind, Kind::New);
        let pushing = ActiveSkill { description: "How we push with git".into(), ..shell };
        assert!(matches!(draft(&c[0], &[pushing], &all, &[]).unwrap().kind, Kind::Edit { .. }));
    }

    #[test]
    fn a_denied_command_head_keeps_only_plain_command_characters() {
        let denied = |task: &str, cmd: &str, ts: i64| Utterance {
            source: Source::Denied { tool: "Bash".into() },
            ..utt_in(A, "api", task, cmd, ts)
        };
        let u: Vec<Utterance> =
            (1..=3).map(|i| denied(&format!("t{i}"), "git`; push<x> --force origin main", i)).collect();
        let c = cluster(&units(&u), &Params::default());
        assert_eq!(c[0].denial.as_ref().map(|d| d.1.as_str()), Some("git pushx"));
        let d = draft(&c[0], &[], &HashMap::new(), &[]).unwrap();
        assert_eq!(d.body.trim_end(), "Do not run `git pushx`; ask the operator first (denied in 3 tasks).");
        // A head with nothing plain left is not mined.
        assert!(units(&[denied("t1", "`;| <>", 1)]).is_empty());
    }

    #[test]
    fn the_newest_units_are_kept_per_tenant_not_across_tenants() {
        // Tenant A's instruction in 3 old tasks; tenant B chatty since.
        let mut u = clippy_three();
        for i in 0..20 {
            u.push(utt_in(
                B,
                "api",
                &format!("b{i}"),
                &format!("deploy the staging cluster number {i} today"),
                10_000 + i,
            ));
        }
        let p = Params { max_units: 5, ..Params::default() };
        let c = cluster(&units(&u), &p);
        assert!(c.iter().any(|c| c.tenant == A && c.tasks.len() == 3), "A's evidence was crowded out: {c:?}");
    }

    #[test]
    fn draft_is_deterministic_and_lints_clean() {
        let c = cluster(&units(&clippy_three()), &Params::default());
        let d1 = draft(&c[0], &[], &HashMap::new(), &[]).unwrap();
        let d2 = draft(&cluster(&units(&clippy_three()), &Params::default())[0], &[], &HashMap::new(), &[]).unwrap();
        assert_eq!(d1, d2);
        assert_eq!(d1.kind, Kind::New);
        assert_eq!(d1.activation, Activation::Always);
        assert_eq!(d1.scope, Scope::repo("github.com/acme/api"), "one repo: repo scope");
        assert!(d1.description.starts_with("Instructions the operator repeated in 3 tasks: "), "{}", d1.description);
        assert_eq!(d1.fingerprint.len(), 16);
        assert!(crate::skills::format::is_name(&d1.name), "{}", d1.name);
        assert!(d1.evidence.len() == 3 && d1.evidence.len() <= 5);
        assert!(!has_errors(&lint(&d1.lint_draft())), "{:?}", lint(&d1.lint_draft()));
        // A name already taken gets -2.
        let d3 = draft(&c[0], &[], &HashMap::new(), std::slice::from_ref(&d1.name)).unwrap();
        assert_eq!(d3.name, format!("{}-2", d1.name));
        // Evidence from two repos of the tenant: org scope.
        let mut u = clippy_three();
        u[1].repo_key = format!("{A}/web");
        let d4 = draft(&cluster(&units(&u), &Params::default())[0], &[], &HashMap::new(), &[]).unwrap();
        assert_eq!(d4.scope, Scope::org(A));
    }

    #[test]
    fn fingerprint_ignores_word_order_and_accents() {
        let a = fingerprint(A, &tokens("Não faça push direto na main"));
        let b = fingerprint(A, &tokens("na main nao faca push direto"));
        assert_eq!(a, b);
        assert_ne!(a, fingerprint(B, &tokens("na main nao faca push direto")));
        assert_eq!(tokens("Não FAÇA push!"), ["nao", "faca", "push"]);
    }
}
