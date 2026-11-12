//! Spec comments that add nothing: one that says what another comment of the
//! same spec says, and one that restates its line or the packaging tool.
//!
//! The agent explains a fact where it applies, once: a dependency on its
//! `BuildRequires`, a skip on the skipped test. A second comment of the same
//! words elsewhere (above `%check`, say) is noise. Only reported: which of
//! the two to keep is the author's call.

use super::comment_compare::is_directive_comment;
use super::comment_macros::comment_lines;
use super::{first_word, is_section};
use crate::quality::{Finding, Kind};
use std::collections::BTreeSet;

/// Words that carry no fact: left out when a `%check` comment is compared
/// with the comment of a dependency.
const STOPWORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "at",
    "least",
    "one",
    "need",
    "needs",
    "with",
    "without",
    "no",
    "not",
    "found",
    "in",
    "of",
    "to",
    "is",
    "are",
    "for",
    "on",
    "and",
    "or",
    "it",
    "this",
    "that",
    "run",
    "runs",
    "running",
    "test",
    "tests",
    "unit",
    "build",
    "building",
    "required",
    "require",
    "package",
    "packages",
    "following",
    "execution",
    "only",
];
/// Lowercase openings of the standard tag comments, which every `PatchN:` or
/// `SourceN:` carries on its own and which are never repeats.
const TAG_STARTS: &[&str] = &["patch-", "source-", "feature-"];
/// Tags whose comment may legitimately say the same above each of several lines.
const PER_LINE_TAGS: &[&str] = &["Provides:", "Obsoletes:", "Conflicts:"];
/// Fewest content words the shorter comment of a pair needs.
const MIN_CONTENT: usize = 3;

/// Fewest words a comment needs to be compared: shorter ones repeat by chance.
const MIN_WORDS: usize = 5;
/// Token-set Jaccard similarity from which two comments are the same.
const SIMILAR: f64 = 0.8;

/// A comment that repeats an earlier one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    /// 1-based line of the later comment.
    pub line: usize,
    /// 1-based line of the earlier comment it repeats.
    pub of: usize,
}

/// The lowercase words of a comment line, each once; URLs are left out.
fn words(line: &str) -> BTreeSet<String> {
    line.trim_start()
        .trim_start_matches('#')
        .split_whitespace()
        .filter(|t| !t.contains("://") && !t.starts_with("www."))
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn similar(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    // Jaccard is at most min/max: sets of very different size cannot match.
    if (a.len().min(b.len()) as f64) < SIMILAR * a.len().max(b.len()) as f64 {
        return false;
    }
    let both = a.intersection(b).count();
    let either = a.len() + b.len() - both;
    either > 0 && both as f64 / either as f64 >= SIMILAR
}

/// Where a line is: the section it is in and the `%if` branches around it.
/// Two comments repeat each other only within one scope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Scope {
    section: usize,
    branches: Vec<usize>,
}

/// The scope of every line of `lines`.
fn scopes(lines: &[&str]) -> Vec<Scope> {
    let mut cur = Scope::default();
    let mut next = 0;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let w = first_word(line);
        if is_section(line) {
            cur.section += 1;
        } else if w.starts_with("%if") {
            next += 1;
            cur.branches.push(next);
        } else if w.starts_with("%el") {
            next += 1;
            if let Some(top) = cur.branches.last_mut() {
                *top = next;
            }
        } else if w == "%endif" {
            cur.branches.pop();
        }
        out.push(cur.clone());
    }
    out
}

/// Whether `line` is a standard tag comment (`# PATCH-FIX-...`).
fn is_tag_comment(line: &str) -> bool {
    let t = line
        .trim_start()
        .trim_start_matches('#')
        .trim_start()
        .to_lowercase();
    TAG_STARTS.iter().any(|p| t.starts_with(p))
}

/// For each line, the index of the first line below it that is not a comment,
/// when that line is a `Patch*`, `Source*`, `Provides`, `Obsoletes` or
/// `Conflicts` tag: the comment explains that line alone.
fn tag_targets(lines: &[&str]) -> Vec<Option<usize>> {
    let mut out = vec![None; lines.len()];
    let mut target = None;
    for i in (0..lines.len()).rev() {
        if !lines[i].trim_start().starts_with('#') {
            let w = first_word(lines[i]);
            let tagged = PER_LINE_TAGS.contains(&w)
                || ["Patch", "Source"]
                    .iter()
                    .any(|p| w.starts_with(p) && w.ends_with(':'));
            target = tagged.then_some(i);
        }
        out[i] = target;
    }
    out
}

/// The comments of `spec` that repeat an earlier comment, in line order. A
/// comment is compared when it has at least five distinct words; directives
/// and lines rpm does not read as comments are left out. Two comments are
/// compared only inside one section and one branch of a conditional. Tag
/// comments, and comments above different `Patch*`, `Source*`, `Provides`,
/// `Obsoletes` or `Conflicts` lines, are not repeats of each other.
pub fn duplicates(spec: &str) -> Vec<Duplicate> {
    let lines: Vec<&str> = spec.lines().collect();
    let real = comment_lines(&lines);
    let scope = scopes(&lines);
    let target = tag_targets(&lines);
    let mut seen: Vec<(usize, BTreeSet<String>)> = Vec::new();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !real[i] || is_directive_comment(line) || is_tag_comment(line) {
            continue;
        }
        let w = words(line);
        if w.len() < MIN_WORDS {
            continue;
        }
        match seen.iter().find(|(at, s)| {
            let apart =
                target[at - 1].is_some() && target[i].is_some() && target[at - 1] != target[i];
            scope[at - 1] == scope[i] && !apart && similar(s, &w)
        }) {
            Some((of, _)) => out.push(Duplicate {
                line: i + 1,
                of: *of,
            }),
            None => seen.push((i + 1, w)),
        }
    }
    for d in check_restates_dependency(&lines, &real, &scope) {
        if !out.iter().any(|o| o.line == d.line) {
            out.push(d);
        }
    }
    out.sort_by_key(|d| d.line);
    out
}

/// The content words of a comment block: lowercase, without stopwords.
fn content_words(block: &[&str]) -> BTreeSet<String> {
    block
        .iter()
        .flat_map(|l| words(l))
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Whether the smaller of two word sets lies inside the other and has enough
/// words to mean something.
fn contained(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small.len() >= MIN_CONTENT && small.is_subset(big)
}

/// Runs of comment lines: (first line index, lines). Lines of a run touch.
fn blocks<'a>(lines: &[&'a str], real: &[bool]) -> Vec<(usize, Vec<&'a str>)> {
    let mut out: Vec<(usize, Vec<&str>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !real[i] || is_directive_comment(line) {
            continue;
        }
        match out.last_mut() {
            Some((at, run)) if *at + run.len() == i => run.push(line),
            _ => out.push((i, vec![line])),
        }
    }
    out
}

/// A `%check` comment that says again what the comment above a `BuildRequires`
/// or `Requires` of the main package says: every content word of the shorter
/// one (at least three, none of them a generic word like "tests") is in the
/// other. Reported on the `%check` comment.
fn check_restates_dependency(lines: &[&str], real: &[bool], scope: &[Scope]) -> Vec<Duplicate> {
    let mut in_check = vec![false; lines.len()];
    let mut check = false;
    for (i, line) in lines.iter().enumerate() {
        if is_section(line) {
            check = first_word(line) == "%check";
        }
        in_check[i] = check;
    }
    let all = blocks(lines, real);
    let deps: Vec<(usize, BTreeSet<String>)> = all
        .iter()
        .filter(|(at, run)| {
            let after = at + run.len();
            scope[*at].section == 0
                && lines
                    .get(after)
                    .is_some_and(|l| matches!(first_word(l), "BuildRequires:" | "Requires:"))
        })
        .map(|(at, run)| (*at + 1, content_words(run)))
        .collect();
    let mut out = Vec::new();
    for (at, run) in all.iter().filter(|(at, _)| in_check[*at]) {
        let w = content_words(run);
        if let Some((of, _)) = deps.iter().find(|(_, d)| contained(&w, d)) {
            out.push(Duplicate {
                line: at + 1,
                of: *of,
            });
        }
    }
    out
}

/// Comment openings that narrate the packaging tool.
const TRIVIAL_STARTS: &[&str] = &["tar_scm names", "obs_scm names"];
/// Phrases that say where the agent found a fact.
const TRIVIAL_PHRASES: &[&str] = &[
    "listed in the upstream installation guide",
    "listed in the readme",
];
/// Restatements of what a BuildRequires is for: trivial when the comment is short.
const RESTATEMENTS: &[&str] = &[
    "needed for building",
    "required for building",
    "build dependency",
    "build-dependency",
];
/// Words from which a restatement says something more.
const SHORT: usize = 8;
/// Most words of a comment a pattern applies to: longer ones carry a reason.
const TRIVIAL_MAX: usize = 12;

/// Whether comment `line` says nothing a maintainer needs: it names where the
/// agent found a fact, narrates the packaging tool, or restates a dependency.
/// A comment with a digit, a version or a comparison is evidence: never trivial.
fn is_trivial(line: &str) -> bool {
    let text = line
        .trim_start()
        .trim_start_matches('#')
        .trim()
        .to_lowercase();
    let n = text.split_whitespace().count();
    if n > TRIVIAL_MAX
        || text
            .chars()
            .any(|c| c.is_ascii_digit() || matches!(c, '<' | '>' | '='))
    {
        return false;
    }
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .collect();
    TRIVIAL_STARTS.iter().any(|p| text.starts_with(p))
        || TRIVIAL_PHRASES.iter().any(|p| text.contains(p))
        || (tokens.contains(&"cc") && text.contains("build scripts compile"))
        || (n < SHORT && RESTATEMENTS.iter().any(|p| text.contains(p)))
}

/// 1-based lines of the trivial comments of `spec`.
pub fn trivial(spec: &str) -> Vec<usize> {
    let lines: Vec<&str> = spec.lines().collect();
    let real = comment_lines(&lines);
    (0..lines.len())
        .filter(|&i| real[i] && !is_directive_comment(lines[i]) && is_trivial(lines[i]))
        .map(|i| i + 1)
        .collect()
}

/// The findings of the comments of `spec`: repeats and trivial ones.
pub fn findings(spec: &str) -> Vec<Finding> {
    let repeats = duplicates(spec).into_iter().map(|d| Finding {
        kind: Kind::DuplicateComment,
        detail: format!(
            "line {}: this comment repeats the one on line {}; keep the one where the fact applies",
            d.line, d.of
        ),
    });
    let trivial = trivial(spec).into_iter().map(|line| Finding {
        kind: Kind::TrivialComment,
        detail: format!(
            "line {line}: this comment restates its line or the packaging tool; remove it unless a maintainer would be surprised without it"
        ),
    });
    repeats.chain(trivial).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_comment_is_reported_against_the_first() {
        let spec = "# the text tests need a system font installed\nBuildRequires: a\n%check\n# The text tests need a system font installed.\n";
        assert_eq!(duplicates(spec), vec![Duplicate { line: 4, of: 1 }]);
    }

    #[test]
    fn one_changed_word_of_ten_is_still_a_duplicate() {
        let spec = "# one two three four five six seven eight nine ten\n# one two three four five six seven eight nine eleven\n";
        assert_eq!(duplicates(spec), vec![Duplicate { line: 2, of: 1 }]);
    }

    #[test]
    fn different_comments_and_short_ones_are_left_alone() {
        let spec =
            "# one two three four five\n# one two three nine ten\n# short one\n# short one\n";
        assert!(duplicates(spec).is_empty());
    }

    #[test]
    fn directives_and_changelog_text_are_not_compared() {
        let spec = "# nodebuginfo\n# nodebuginfo\n%changelog\n# one two three four five six\n# one two three four five six\n";
        assert!(duplicates(spec).is_empty());
    }

    const PLEAMAR: &str = "# the text tests need at least one system font\nBuildRequires: dejavu-fonts\n\n%check\n# text tests panic with \"no default font found\" without a system font\n%{cargo_test}\n";

    #[test]
    fn a_check_comment_restating_a_dependency_comment_is_reported() {
        assert_eq!(duplicates(PLEAMAR), vec![Duplicate { line: 5, of: 1 }]);
    }

    #[test]
    fn unrelated_check_and_dependency_comments_are_left_alone() {
        let spec = "# the network tests need curl installed\nBuildRequires: curl\n%check\n# skip the slow benchmark tests here\n%{cargo_test}\n";
        assert!(duplicates(spec).is_empty());
        let spec = "# needs a font\nBuildRequires: f\n%check\n# a font panic\n%{cargo_test}\n";
        assert!(duplicates(spec).is_empty());
    }

    #[test]
    fn the_same_comment_in_two_subpackages_is_not_a_repeat() {
        let c = "# one two three four five six";
        let spec = format!("%package a\n{c}\nSummary: a\n%package b\n{c}\nSummary: b\n");
        assert!(duplicates(&spec).is_empty());
        let spec = format!("%package a\n{c}\n{c}\n");
        assert_eq!(duplicates(&spec), vec![Duplicate { line: 3, of: 2 }]);
    }

    #[test]
    fn the_same_comment_in_both_branches_of_a_conditional_is_not_a_repeat() {
        let c = "# one two three four five six";
        let spec = format!("%if 0%{{?x}}\n{c}\nA: 1\n%else\n{c}\nA: 2\n%endif\n");
        assert!(duplicates(&spec).is_empty());
        let spec = format!("%if 0%{{?x}}\n{c}\n{c}\n%endif\n");
        assert_eq!(duplicates(&spec).len(), 1);
    }

    #[test]
    fn a_comment_much_longer_than_another_is_not_compared() {
        let long = "# a b c d e f g h i j k l m n o p q r s t";
        let spec = format!("# a b c d e\n{long}\n");
        assert!(duplicates(&spec).is_empty());
    }

    #[test]
    fn the_sample_specs_have_no_duplicate_comment() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/spec");
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let spec = std::fs::read_to_string(&p).unwrap();
            assert!(duplicates(&spec).is_empty(), "{}", p.display());
        }
    }

    #[test]
    fn the_generated_trivial_comments_are_flagged() {
        let spec = "# Libraries listed in the upstream installation guide\nBuildRequires: a\n# the cc build-dependency and the build scripts compile C code\nBuildRequires: gcc\n# tar_scm names the archive after the repository (jay), not the package\nSource: x\n# needed for building\nBuildRequires: b\n";
        assert_eq!(trivial(spec), vec![1, 3, 5, 7]);
    }

    #[test]
    fn evidence_and_reason_comments_are_not_trivial() {
        let spec = "# build scripts compile bundled zstd, so we need gcc and make\n# Libraries listed in the manifest are vendored\n# listed in the README as a runtime requirement, needs gtk4 >= 4.10\n# unverified: src/lib.rs:12 includes zlib.h\n# skipped: needs a display, upstream issue 42\n";
        assert_eq!(trivial(spec), Vec::<usize>::new());
    }

    #[test]
    fn the_sample_specs_have_no_trivial_comment() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/spec");
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let spec = std::fs::read_to_string(&p).unwrap();
            assert!(trivial(&spec).is_empty(), "{}", p.display());
        }
    }
}
