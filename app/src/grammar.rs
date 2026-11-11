//! Offline spelling, grammar, and style checking via Harper.
//!
//! Everything runs locally on the sheet text: no network, no account, no
//! text ever leaves the machine. Harper lints Markdown directly, and its
//! spans are char offsets into the same string the editor edits, so they
//! map 1:1 onto egui caret positions.

use std::collections::BTreeSet;
use std::sync::Arc;

use harper_core::linting::{Lint, LintGroup, LintKind, Linter, Suggestion};
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{Dialect, DictWordMetadata, Document};

/// Docs above this size are skipped per lint pass (perf guard).
pub const MAX_LINT_CHARS: usize = 100_000;

/// Where an issue surfaces: inline squiggles (spelling + grammar) or the
/// style sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IssueKind {
    Spelling,
    Grammar,
    Style,
}

/// Harper's style-adjacent lints live in the sidebar; everything else is
/// inline. Spelling vs. typo both read as spelling to the user.
pub fn issue_kind_of(kind: LintKind) -> IssueKind {
    match kind {
        LintKind::Style | LintKind::Enhancement | LintKind::Readability | LintKind::Redundancy => {
            IssueKind::Style
        }
        LintKind::Spelling | LintKind::Typo => IssueKind::Spelling,
        _ => IssueKind::Grammar,
    }
}

/// One user-facing issue: char offsets into the sheet content plus the
/// message and applicable fixes.
#[derive(Clone, Debug)]
pub struct Issue {
    pub start: usize,
    pub end: usize,
    pub kind: IssueKind,
    pub message: String,
    pub suggestions: Vec<AppliedSuggestion>,
    /// Covered text, for the session ignore-key.
    pub covered: String,
}

#[derive(Clone, Debug)]
pub enum AppliedSuggestion {
    Replace(String),
    InsertAfter(String),
    Remove,
}

impl AppliedSuggestion {
    fn from_suggestion(s: &Suggestion) -> Self {
        match s {
            Suggestion::ReplaceWith(chars) => {
                AppliedSuggestion::Replace(chars.iter().collect())
            }
            Suggestion::InsertAfter(chars) => {
                AppliedSuggestion::InsertAfter(chars.iter().collect())
            }
            Suggestion::Remove => AppliedSuggestion::Remove,
        }
    }
}

/// Apply a suggestion to `content` at char offsets `[start, end)`.
/// Returns `None` when the offsets are out of bounds.
pub fn apply_at(
    content: &str,
    start: usize,
    end: usize,
    suggestion: &AppliedSuggestion,
) -> Option<String> {
    fn byte_of(content: &str, char_idx: usize) -> Option<usize> {
        if char_idx == content.chars().count() {
            return Some(content.len());
        }
        content
            .char_indices()
            .nth(char_idx)
            .map(|(byte, _)| byte)
    }
    let (bs, be) = (byte_of(content, start)?, byte_of(content, end)?);
    if bs > be {
        return None;
    }
    let mut out = String::with_capacity(content.len() + 16);
    out.push_str(&content[..bs]);
    match suggestion {
        AppliedSuggestion::Replace(text) => out.push_str(text),
        AppliedSuggestion::InsertAfter(text) => {
            out.push_str(&content[bs..be]);
            out.push_str(text);
        }
        AppliedSuggestion::Remove => {}
    }
    out.push_str(&content[be..]);
    Some(out)
}

/// Char range of a lint span as a string slice, for ignore keys and excerpts.
pub fn span_text(content: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    let end = end.min(chars.len());
    let start = start.min(end);
    chars[start..end].iter().collect()
}

/// Effective custom dictionary: additions minus removals, sorted.
pub fn effective_words(adds: &[String], removes: &[String]) -> BTreeSet<String> {
    let blocked: BTreeSet<&str> = removes.iter().map(|s| s.as_str()).collect();
    adds.iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
        .filter(|s| !blocked.contains(*s))
        .map(|s| s.to_string())
        .collect()
}

/// Union-merge remote word sets into local ones (both directions converge).
pub fn merge_word_sets(
    local_adds: &mut BTreeSet<String>,
    local_removes: &mut BTreeSet<String>,
    remote_adds: &[String],
    remote_removes: &[String],
) {
    local_adds.extend(remote_adds.iter().cloned());
    local_removes.extend(remote_removes.iter().cloned());
}

/// Fingerprint of the effective dictionary; the app rebuilds the checker
/// only when this changes.
pub fn dictionary_fingerprint(words: &BTreeSet<String>) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for w in words {
        for b in w.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Harper wrapper: curated dictionary merged with the user's words.
pub struct Checker {
    dictionary: Arc<MergedDictionary>,
    linter: LintGroup,
}

impl Checker {
    pub fn new(custom_words: &BTreeSet<String>) -> Self {
        let mut custom = MutableDictionary::new();
        for w in custom_words {
            custom.append_word_str(w, DictWordMetadata::default());
        }
        let mut merged = MergedDictionary::new();
        merged.add_dictionary(FstDictionary::curated());
        merged.add_dictionary(Arc::new(custom));
        let dictionary = Arc::new(merged);
        let linter = LintGroup::new_curated(dictionary.clone(), Dialect::American);
        Self {
            dictionary,
            linter,
        }
    }

    /// Lint `text`, skipping ignored `(message, covered)` pairs.
    /// Returns issues sorted by position.
    pub fn lint(
        &mut self,
        text: &str,
        ignored: &std::collections::HashSet<(String, String)>,
    ) -> Vec<Issue> {
        if text.chars().count() > MAX_LINT_CHARS {
            return Vec::new();
        }
        let document = Document::new_markdown_default(text, &self.dictionary);
        let mut lints = self.linter.lint(&document);
        lints.sort_by_key(|l| (l.span.start, l.span.end));
        lints
            .iter()
            .filter_map(|l| Issue::from_lint(l, text, ignored))
            .collect()
    }
}

impl Issue {
    fn from_lint(
        lint: &Lint,
        text: &str,
        ignored: &std::collections::HashSet<(String, String)>,
    ) -> Option<Self> {
        if lint.span.end <= lint.span.start {
            return None;
        }
        let covered = span_text(text, lint.span.start, lint.span.end);
        if covered.trim().is_empty() {
            return None;
        }
        if ignored.contains(&(lint.message.clone(), covered.clone())) {
            return None;
        }
        Some(Self {
            start: lint.span.start,
            end: lint.span.end,
            kind: issue_kind_of(lint.lint_kind),
            message: lint.message.clone(),
            suggestions: lint
                .suggestions
                .iter()
                .map(AppliedSuggestion::from_suggestion)
                .collect(),
            covered,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Vec<Issue> {
        let mut checker = Checker::new(&BTreeSet::new());
        checker.lint(text, &std::collections::HashSet::new())
    }

    #[test]
    fn issue_kinds_split_inline_and_sidebar() {
        assert_eq!(issue_kind_of(LintKind::Spelling), IssueKind::Spelling);
        assert_eq!(issue_kind_of(LintKind::Typo), IssueKind::Spelling);
        assert_eq!(issue_kind_of(LintKind::Grammar), IssueKind::Grammar);
        assert_eq!(issue_kind_of(LintKind::Agreement), IssueKind::Grammar);
        assert_eq!(issue_kind_of(LintKind::Style), IssueKind::Style);
        assert_eq!(issue_kind_of(LintKind::Enhancement), IssueKind::Style);
        assert_eq!(issue_kind_of(LintKind::Readability), IssueKind::Style);
        assert_eq!(issue_kind_of(LintKind::Redundancy), IssueKind::Style);
    }

    #[test]
    fn clean_text_has_no_issues() {
        assert!(check("The quick brown fox jumps over the lazy dog.\n").is_empty());
    }

    #[test]
    fn spelling_error_is_found_with_fix() {
        let issues = check("Teh quick brown fox.\n");
        let spelling: Vec<_> = issues
            .iter()
            .filter(|i| i.kind == IssueKind::Spelling)
            .collect();
        assert!(!spelling.is_empty(), "expected a spelling hit");
        let first = spelling[0];
        assert_eq!(&first.covered, "Teh");
        assert!(
            first.suggestions.iter().any(|s| matches!(
                s,
                AppliedSuggestion::Replace(t) if t == "The"
            )),
            "suggestions: {:?}",
            first.suggestions
        );
        // Span points at the word in the source text.
        assert_eq!(span_text("Teh quick brown fox.\n", first.start, first.end), "Teh");
    }

    #[test]
    fn custom_dictionary_suppresses_word() {
        // A surname unlikely to be in the curated dictionary.
        let text = "Royleance is here.\n";
        assert!(check(text)
            .iter()
            .any(|i| i.kind == IssueKind::Spelling && i.covered == "Royleance"));
        let mut words = BTreeSet::new();
        words.insert("Royleance".to_string());
        let mut checker = Checker::new(&words);
        assert!(!check_with(&mut checker, text)
            .iter()
            .any(|i| i.covered == "Royleance"));
    }

    fn check_with(checker: &mut Checker, text: &str) -> Vec<Issue> {
        checker.lint(text, &std::collections::HashSet::new())
    }

    #[test]
    fn apply_replace_insert_remove() {
        let text = "Teh cat sat.";
        let out = apply_at(
            text,
            0,
            3,
            &AppliedSuggestion::Replace("The".to_string()),
        )
        .unwrap();
        assert_eq!(out, "The cat sat.");
        let out = apply_at(text, 0, 3, &AppliedSuggestion::Remove).unwrap();
        assert_eq!(out, " cat sat.");
        let out = apply_at(
            text,
            3,
            3,
            &AppliedSuggestion::InsertAfter("!".to_string()),
        )
        .unwrap();
        assert_eq!(out, "Teh! cat sat.");
        // Unicode-safe: char offsets, not bytes.
        let uni = "Héllo wörld";
        let out = apply_at(
            uni,
            6,
            11,
            &AppliedSuggestion::Replace("world".to_string()),
        )
        .unwrap();
        assert_eq!(out, "Héllo world");
        assert!(apply_at(text, 0, 99, &AppliedSuggestion::Remove).is_none());
        assert!(apply_at(text, 5, 2, &AppliedSuggestion::Remove).is_none());
    }

    #[test]
    fn effective_words_subtracts_removals() {
        let adds = vec!["Ariadne".to_string(), "  ".to_string(), "two words".to_string()];
        let removes = vec!["Ariadne".to_string()];
        let eff = effective_words(&adds, &removes);
        assert!(eff.is_empty());
        let eff = effective_words(&["Ariadne".to_string()], &[]);
        assert!(eff.contains("Ariadne"));
    }

    #[test]
    fn merge_word_sets_unions_both_sides() {
        let mut adds = BTreeSet::from(["a".to_string()]);
        let mut removes = BTreeSet::from(["x".to_string()]);
        merge_word_sets(&mut adds, &mut removes, &["b".to_string()], &["y".to_string()]);
        assert!(adds.contains("a") && adds.contains("b") && adds.len() == 2);
        assert!(removes.contains("x") && removes.contains("y") && removes.len() == 2);
        // Idempotent.
        merge_word_sets(&mut adds, &mut removes, &["b".to_string()], &["y".to_string()]);
        assert_eq!(adds.len(), 2);
    }

    #[test]
    fn ignored_pairs_are_skipped() {
        let issues = check("Teh quick brown fox.\n");
        assert!(!issues.is_empty());
        let first = &issues[0];
        let mut ignored = std::collections::HashSet::new();
        ignored.insert((first.message.clone(), first.covered.clone()));
        let mut checker = Checker::new(&BTreeSet::new());
        let again = checker.lint("Teh quick brown fox.\n", &ignored);
        assert!(!again.iter().any(|i| i.covered == first.covered
            && i.message == first.message));
    }
}
