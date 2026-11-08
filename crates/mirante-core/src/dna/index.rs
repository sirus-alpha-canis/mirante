//! The daemon's regenerable index of `.mirante/memory` (notes, anchors, links, FTS5).

/// Most terms a search uses.
const MAX_QUERY_TERMS: usize = 12;

/// A safe FTS5 `MATCH` expression for free text: alphanumeric runs, lowercased, each a
/// quoted prefix term (`"term"*`), joined with `OR`, at most 12. FTS5 syntax in the input
/// (`NEAR`, `-`, `:`, quotes, `*`) is dropped. `None` when no term is left.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_QUERY_TERMS)
        .map(|t| format!("\"{}\"*", t.to_lowercase()))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

#[cfg(test)]
mod fts_tests {
    use super::*;

    #[test]
    fn fts_query_quotes_terms_and_drops_syntax() {
        assert_eq!(fts_query("hook spool").as_deref(), Some("\"hook\"* OR \"spool\"*"));
        assert_eq!(
            fts_query("NEAR(a b) OR \"x\" -y title:z").as_deref(),
            Some("\"near\"* OR \"a\"* OR \"b\"* OR \"or\"* OR \"x\"* OR \"y\"* OR \"title\"* OR \"z\"*")
        );
        assert_eq!(fts_query("  ** ").as_deref(), None);
        assert_eq!(fts_query(&"w ".repeat(50)).unwrap().matches(" OR ").count(), 11);
    }
}
