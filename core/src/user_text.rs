//! Replacing references to the repository's internal rules in text that a
//! user reads. A reference becomes neutral wording, so the sentence keeps its
//! grammar. The raw text (run.jsonl, `explain`) keeps the original.

/// `text` with references to the internal conventions replaced by neutral
/// wording, in one left-to-right pass. Text without such a reference is
/// returned unchanged.
///
/// A reference must start the text or follow whitespace, `(`, `[`, `"`, `'`
/// or `,`. Each one becomes `the packaging guideline`, or `the packaging
/// guidelines` for `conventions` or a list of several numbers, with the first
/// letter capitalised at the start of a sentence:
/// - `convention N`, `conventions N, M and K`, `conventions N-M`, with `the`
///   before it dropped (`the convention 5` becomes `the packaging
///   guideline`). A later list item has one to three digits and is not
///   followed by a quantity word (see `list_item_follows`). A possessive
///   `'s` is kept;
/// - `conventions/N.md` and `build/conventions/N.md`, unless preceded by `/`,
///   `-`, `.`, `:` or a word character, or followed by a longer path;
/// - `section N of convention M` and `section N of the conventions`, which
///   become the noun alone.
///
/// Directly after a reference, a `(section M ...)` group, `, section M` and
/// ` section M` (not followed by of, in or from) are part of what is replaced,
/// since the noun says it. A `(section N)` that does not follow a reference is
/// kept.
///
/// Known limit: `convention N` always means the internal rules in this
/// codebase, so `convention 3 files` becomes `the packaging guideline files`.
pub fn scrub_internal_refs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // `text[..copied]` is already in `out`.
    let mut copied = 0;
    let mut i = 0;
    while i < text.len() {
        match rewrite_at(text, i) {
            Some((end, noun)) => {
                out.push_str(&text[copied..i]);
                push_noun(&mut out, noun);
                copied = end;
                i = end;
            }
            None => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// What a reference is replaced with.
#[derive(Clone, Copy)]
struct Noun {
    plural: bool,
    capital: bool,
    possessive: bool,
}

/// A reference found by one of the matchers: where it ends and what it is.
#[derive(Clone, Copy)]
struct Found {
    end: usize,
    plural: bool,
    possessive: bool,
}

fn push_noun(out: &mut String, noun: Noun) {
    out.push_str(match (noun.capital, noun.plural) {
        (false, false) => "the packaging guideline",
        (false, true) => "the packaging guidelines",
        (true, false) => "The packaging guideline",
        (true, true) => "The packaging guidelines",
    });
    if noun.possessive {
        out.push_str("'s");
    }
}

/// The reference starting at byte `i` of `text`, as the end of the text it
/// covers and the noun that replaces it.
fn rewrite_at(text: &str, i: usize) -> Option<(usize, Noun)> {
    if !boundary_before(text, i) {
        return None;
    }
    match text.as_bytes()[i] {
        b'c' | b'C' => convention_ref(text, i)
            .or_else(|| path(text, i))
            .map(|f| render(text, i, f)),
        b'b' | b'B' => path(text, i).map(|f| render(text, i, f)),
        b't' | b'T' => the_ref(text, i).map(|f| render(text, i, f)),
        b's' | b'S' => section_of(text, i),
        _ => None,
    }
}

fn render(text: &str, at: usize, f: Found) -> (usize, Noun) {
    let capital = text.as_bytes()[at].is_ascii_uppercase() && sentence_start(text, at);
    (
        f.end,
        Noun {
            plural: f.plural,
            capital,
            possessive: f.possessive,
        },
    )
}

/// Whether the character before `i` lets a reference start at `i`.
fn boundary_before(text: &str, i: usize) -> bool {
    text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\'' | ','))
}

/// Whether `i` starts a sentence: nothing, or a sentence end, before it.
fn sentence_start(text: &str, i: usize) -> bool {
    text[..i]
        .trim_end_matches(is_blank)
        .chars()
        .next_back()
        .is_none_or(|c| matches!(c, '.' | '!' | '?' | ':' | ';' | '\n'))
}

/// `the convention N` or `the conventions/N.md`, with the article in the
/// reference, so the noun supplies it.
fn the_ref(text: &str, i: usize) -> Option<Found> {
    let k = keyword(text, i, "the")?;
    let c = skip_blanks(text.as_bytes(), k);
    if c == k {
        return None;
    }
    convention_ref(text, c).or_else(|| path(text, c))
}

/// `section N of convention M` or `section N of the conventions`. The
/// section part is dropped; the noun is singular for `the conventions`.
fn section_of(text: &str, i: usize) -> Option<(usize, Noun)> {
    let b = text.as_bytes();
    let end = section_ref(text, i)?;
    let of = skip_blanks(b, end);
    if of == end {
        return None;
    }
    let q = keyword(text, of, "of")?;
    let r = skip_blanks(b, q);
    if r == q {
        return None;
    }
    if let Some(t) = keyword(text, r, "the") {
        let c = skip_blanks(b, t);
        let e = keyword(text, c, "conventions").filter(|_| c > t)?;
        let f = Found {
            end: e,
            plural: false,
            possessive: false,
        };
        return Some(render(text, r, f));
    }
    let f = convention_ref(text, r)?;
    Some(render(text, r, f))
}

/// `convention N ...` or `conventions N ...` at `i`, with the section group
/// or follow-on that goes with it.
fn convention_ref(text: &str, i: usize) -> Option<Found> {
    let b = text.as_bytes();
    let (k, plural_word) = match keyword(text, i, "conventions") {
        Some(k) => (k, true),
        None => (keyword(text, i, "convention")?, false),
    };
    let n = skip_blanks(b, k);
    if n == k {
        return None;
    }
    let (end, items) = list(text, n)?;
    let plural = plural_word || items > 1;
    if let Some(e) = paren_section(text, skip_blanks(b, end)) {
        return Some(with_possessive(text, e, plural));
    }
    if let Some(e) = follow_on(text, end) {
        return Some(Found {
            end: e,
            plural,
            possessive: false,
        });
    }
    Some(with_possessive(text, end, plural))
}

fn with_possessive(text: &str, end: usize, plural: bool) -> Found {
    match possessive(text, end) {
        Some(e) => Found {
            end: e,
            plural,
            possessive: true,
        },
        None => Found {
            end,
            plural,
            possessive: false,
        },
    }
}

/// `conventions/N.md` at `i`, with `build/` optionally in front. Not a longer
/// path: nothing that continues it with `/` or a word follows.
fn path(text: &str, i: usize) -> Option<Found> {
    let k = prefix(text, i, "build/").unwrap_or(i);
    let k = prefix(text, k, "conventions/")?;
    let k = number(text, k)?;
    let k = prefix(text, k, ".md")?;
    let rest = text.get(k..)?;
    let whole = match rest.chars().next() {
        None => true,
        Some('/') => false,
        Some('.') => !rest[1..].chars().next().is_some_and(is_word_char),
        Some(c) => !is_word_char(c),
    };
    whole.then_some(Found {
        end: k,
        plural: true,
        possessive: false,
    })
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn skip_blanks(b: &[u8], j: usize) -> usize {
    j + b.get(j..).map_or(0, |s| {
        s.iter().take_while(|&&c| c == b' ' || c == b'\t').count()
    })
}

fn word_end(text: &str, end: usize) -> bool {
    text.get(end..)
        .and_then(|s| s.chars().next())
        .is_none_or(|c| !is_word_char(c))
}

/// The end of `word` at `j`, ASCII case-insensitively, with no word character
/// required after it.
fn prefix(text: &str, j: usize, word: &str) -> Option<usize> {
    let end = j.checked_add(word.len())?;
    text.as_bytes()
        .get(j..end)?
        .eq_ignore_ascii_case(word.as_bytes())
        .then_some(end)
}

fn keyword(text: &str, j: usize, word: &str) -> Option<usize> {
    prefix(text, j, word).filter(|&end| word_end(text, end))
}

/// A run of digits at `j` that is not followed by a word character.
fn number(text: &str, j: usize) -> Option<usize> {
    let digits = text
        .as_bytes()
        .get(j..)?
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let end = j + digits;
    (digits > 0 && word_end(text, end)).then_some(end)
}

/// A separator between two numbers of a list, ending after it.
fn separator(text: &str, k: usize) -> Option<usize> {
    let rest = text.get(k..)?;
    if rest.starts_with([',', '&', '-']) {
        return Some(k + 1);
    }
    if rest.starts_with('–') {
        return Some(k + '–'.len_utf8());
    }
    keyword(text, k, "and").or_else(|| keyword(text, k, "or"))
}

/// A list item after a separator: one to three digits, then a word boundary.
/// Four digits are a year or a count, not a convention number.
fn item(text: &str, j: usize) -> Option<usize> {
    let end = number(text, j)?;
    (end - j <= 3).then_some(end)
}

/// Words that may follow a list item and show it is a convention number.
const LIST_FOLLOWERS: &str = "and or apply applies are is of in for to says section \
    sections convention conventions hold holds be as which that with from on by";

/// Whether a list item ending at `end` may stay in the list. It may when
/// nothing or punctuation follows it, or one of `LIST_FOLLOWERS`. So
/// `convention 3, 2 files` ends the list at `3` (`files` is a quantity), and
/// `conventions 3, 4 apply` takes `4`.
fn list_item_follows(text: &str, end: usize) -> bool {
    let p = skip_blanks(text.as_bytes(), end);
    match text.get(p..).and_then(|s| s.chars().next()) {
        None => true,
        Some(c) if !is_word_char(c) => true,
        Some(_) => LIST_FOLLOWERS
            .split_whitespace()
            .any(|w| keyword(text, p, w).is_some()),
    }
}

/// `N`, `N, M`, `N and M`, `N-M` and so on, starting at `j`, with the number
/// of items. The first number may have any length; each later item must pass
/// `item` and `list_item_follows`, and a list stops before the first item
/// that does not.
fn list(text: &str, j: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let mut end = number(text, j)?;
    let mut items = 1;
    while let Some(sep) = separator(text, skip_blanks(b, end)) {
        let Some(next) = item(text, skip_blanks(b, sep)) else {
            break;
        };
        if !list_item_follows(text, next) {
            break;
        }
        end = next;
        items += 1;
    }
    Some((end, items))
}

/// `section N ...` or `sections N, M ...` at `j`.
fn section_ref(text: &str, j: usize) -> Option<usize> {
    let k = keyword(text, j, "sections").or_else(|| keyword(text, j, "section"))?;
    list(text, skip_blanks(text.as_bytes(), k)).map(|(end, _)| end)
}

/// `(section N ...)` at `j`, with nothing but the list inside the parentheses.
fn paren_section(text: &str, j: usize) -> Option<usize> {
    let b = text.as_bytes();
    if b.get(j).copied() != Some(b'(') {
        return None;
    }
    let end = section_ref(text, skip_blanks(b, j + 1))?;
    let close = skip_blanks(b, end);
    (b.get(close).copied() == Some(b')')).then_some(close + 1)
}

/// Whether a `section` reference ending at `end` is followed by a document
/// name (`section 3 of the manual`), which makes it prose.
fn document_follows(text: &str, end: usize) -> bool {
    let p = skip_blanks(text.as_bytes(), end);
    ["of", "in", "from"]
        .iter()
        .any(|w| keyword(text, p, w).is_some())
}

/// `section N` directly after a convention reference ending at `end`, with an
/// optional separator in between. Returns where that section reference ends.
fn follow_on(text: &str, end: usize) -> Option<usize> {
    let b = text.as_bytes();
    let p = skip_blanks(b, end);
    let p = match b.get(p).copied() {
        Some(b',' | b';') => skip_blanks(b, p + 1),
        _ => match keyword(text, p, "and").or_else(|| keyword(text, p, "or")) {
            Some(e) => skip_blanks(b, e),
            None => p,
        },
    };
    let e = section_ref(text, p)?;
    (!document_follows(text, e)).then_some(e)
}

/// `'s` or `’s` after a reference, ending at `end`.
fn possessive(text: &str, end: usize) -> Option<usize> {
    let rest = text.get(end..)?;
    let len = if rest.starts_with("'s") {
        2
    } else if rest.starts_with("’s") {
        "’s".len()
    } else {
        return None;
    };
    let e = end + len;
    word_end(text, e).then_some(e)
}
