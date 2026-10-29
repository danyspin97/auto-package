use super::*;

/// Whether a `[build-system] requires` entry is a PEP 508 requirement a spec
/// can carry verbatim inside `%{python_module ...}`: a distribution name, an
/// optional `[extras]` list, and optional comma-separated version clauses
/// (`setuptools >= 42, < 80`). Markers, URLs, parentheses and anything that
/// could open a spec line or a macro (newline, `%`, `{`, `}`, `$`, quotes,
/// backticks, `;`, `|`, `&`, backslash) are refused.
pub fn is_safe_build_requirement(req: &str) -> bool {
    if req.len() > 200 || !req.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return false;
    }
    let req = req.trim();
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    let name_end = req.find(|c: char| !is_word(c)).unwrap_or(req.len());
    let name = &req[..name_end];
    let starts_alnum = |s: &str| s.starts_with(|c: char| c.is_ascii_alphanumeric());
    if name.is_empty()
        || !starts_alnum(name)
        || !name.ends_with(|c: char| c.is_ascii_alphanumeric())
    {
        return false;
    }
    let mut rest = req[name_end..].trim_start();
    if let Some(inner) = rest.strip_prefix('[') {
        let Some(end) = inner.find(']') else {
            return false;
        };
        let extras_ok = inner[..end].split(',').all(|e| {
            let e = e.trim();
            starts_alnum(e) && e.chars().all(is_word)
        });
        if !extras_ok {
            return false;
        }
        rest = inner[end + 1..].trim_start();
    }
    if rest.is_empty() {
        return true;
    }
    // The version clauses, bare or in one pair of parentheses (`foo (>=1, <2)`).
    let clauses = match rest.strip_prefix('(') {
        Some(inner) => match inner.strip_suffix(')') {
            Some(clauses) => clauses,
            None => return false,
        },
        None => rest,
    };
    clauses
        .split(',')
        .all(|clause| split_version_clause(clause).is_some())
}

/// The operator and the version of one PEP 440 version clause (`>= 42`), both
/// trimmed, when the clause is one a spec can carry: a version of letters,
/// digits and `. * + ! _ -`, starting with a letter or digit.
fn split_version_clause(clause: &str) -> Option<(&str, &str)> {
    const OPS: [&str; 8] = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"];
    let clause = clause.trim();
    let op = OPS.iter().find(|op| clause.starts_with(**op))?;
    let version = clause[op.len()..].trim();
    let ok = version.starts_with(|c: char| c.is_ascii_alphanumeric())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '*' | '+' | '!' | '_' | '-'));
    ok.then_some((*op, version))
}

/// The requirement a spec writes for a requirement that passed
/// [`is_safe_build_requirement`]. A parenthesized version list is written
/// without its parentheses, as `%{python_module}` needs (`foo (>=1, <2)` is
/// `foo >= 1, < 2`); anything else is written as it is.
fn spec_requirement(req: &str) -> Cow<'_, str> {
    let Some(open) = req.find('(') else {
        return Cow::Borrowed(req);
    };
    let head = req[..open].trim_end();
    let inner = &req[open + 1..];
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    let clauses: Vec<String> = inner
        .split(',')
        .filter_map(split_version_clause)
        .map(|(op, version)| format!("{op} {version}"))
        .collect();
    Cow::Owned(format!("{head} {}", clauses.join(", ")))
}

/// What a `[build-system] requires` entry means for a spec.
#[derive(Debug, PartialEq, Eq)]
pub enum BuildRequirement<'a> {
    /// The requirement part, to be written as a `BuildRequires`. A marker that
    /// is undecided for the build target, or cannot be parsed, is kept with it.
    /// Borrowed unless the parentheses of a version list had to be removed.
    Keep(Cow<'a, str>),
    /// The environment marker is false for the build target (Linux, CPython,
    /// x86_64 or aarch64), so the requirement is not installed.
    NotForTarget,
}

/// What a `[build-system] requires` entry contributes to a spec: its requirement
/// part, and whether its PEP 508 environment marker (`; sys_platform == "win32"`)
/// rules it out for the build target (see [`marker_excludes_target`]). `None`
/// when the requirement part is not plain, or the marker holds a character a
/// spec or shell could expand (see [`is_safe_marker`]).
pub fn build_requirement_part(entry: &str) -> Option<BuildRequirement<'_>> {
    let (req, marker) = match entry.split_once(';') {
        Some((req, marker)) => (req, Some(marker)),
        None => (entry, None),
    };
    if marker.is_some_and(|m| !is_safe_marker(m)) {
        return None;
    }
    let req = req.trim();
    if !is_safe_build_requirement(req) {
        return None;
    }
    if marker.is_some_and(marker_excludes_target) {
        return Some(BuildRequirement::NotForTarget);
    }
    Some(BuildRequirement::Keep(spec_requirement(req)))
}

/// A token of a PEP 508 marker.
#[derive(Debug, PartialEq)]
enum MarkerTok<'a> {
    /// A marker variable, or `and`, `or`, `in`, `not`.
    Word(&'a str),
    /// A quoted value, without its quotes.
    Str(&'a str),
    /// A run of comparison characters (`==`, `!=`, `<=`, ...).
    Op(&'a str),
    /// `(`.
    Open,
    /// `)`.
    Close,
}

/// Splits a marker into tokens; `None` on anything else (a lone quote, a stray
/// character).
fn tokenize_marker(marker: &str) -> Option<Vec<MarkerTok<'_>>> {
    let is_op = |c: char| "=!<>~".contains(c);
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.');
    let mut out = Vec::new();
    let mut rest = marker;
    loop {
        rest = rest.trim_start();
        let Some(c) = rest.chars().next() else {
            return Some(out);
        };
        if c == '\'' || c == '"' {
            let inner = &rest[1..];
            let end = inner.find(c)?;
            out.push(MarkerTok::Str(&inner[..end]));
            rest = &inner[end + 1..];
        } else if is_op(c) {
            let end = rest.find(|ch: char| !is_op(ch)).unwrap_or(rest.len());
            out.push(MarkerTok::Op(&rest[..end]));
            rest = &rest[end..];
        } else if is_word(c) {
            let end = rest.find(|ch: char| !is_word(ch)).unwrap_or(rest.len());
            out.push(MarkerTok::Word(&rest[..end]));
            rest = &rest[end..];
        } else if c == '(' || c == ')' {
            out.push(if c == '(' {
                MarkerTok::Open
            } else {
                MarkerTok::Close
            });
            rest = &rest[1..];
        } else {
            return None;
        }
    }
}

/// The value a marker variable has for the build target (Linux, CPython on
/// x86_64 or aarch64), for the variables that do not depend on the machine or
/// the Python version. `None` for every other variable: undecided.
fn target_marker_value(var: &str) -> Option<&'static str> {
    match var {
        "sys_platform" | "sys.platform" => Some("linux"),
        "os_name" | "os.name" => Some("posix"),
        "platform_system" => Some("Linux"),
        "implementation_name" => Some("cpython"),
        "platform_python_implementation" | "platform.python_implementation" => Some("CPython"),
        _ => None,
    }
}

/// The verdict of one `<operand> <op> <operand>` clause at the start of
/// `toks`, and how many tokens it used. `None` in the verdict is undecided; the
/// whole result is `None` when the clause is not of that form.
fn marker_clause_verdict(toks: &[MarkerTok<'_>]) -> Option<(Option<bool>, usize)> {
    let (lhs, op, rhs, used) = match toks {
        [a, MarkerTok::Word("not"), MarkerTok::Word("in"), b, ..] => (a, "not in", b, 4),
        [a, MarkerTok::Word("in"), b, ..] => (a, "in", b, 3),
        [a, MarkerTok::Op(op), b, ..] => (a, *op, b, 3),
        _ => return None,
    };
    let (var, value, var_left) = match (lhs, rhs) {
        (MarkerTok::Word(v), MarkerTok::Str(s)) => (*v, *s, true),
        (MarkerTok::Str(s), MarkerTok::Word(v)) => (*v, *s, false),
        _ => return None,
    };
    let Some(target) = target_marker_value(var) else {
        return Some((None, used));
    };
    let verdict = match op {
        "==" | "===" => Some(value == target),
        "!=" => Some(value != target),
        // `'x' in var` asks whether x is inside var; `var in 'x'` whether var is
        // inside the quoted list.
        "in" | "not in" => {
            let found = if var_left {
                value.contains(target)
            } else {
                target.contains(value)
            };
            Some(found == (op == "in"))
        }
        _ => None,
    };
    Some((verdict, used))
}

/// The truth of a marker for the build target: `Some(true)` or `Some(false)`,
/// or `None` when a clause the target cannot decide makes it unknown.
type Verdict = Option<bool>;

/// Kleene `and`: false if either side is false, true if both are true.
fn verdict_and(a: Verdict, b: Verdict) -> Verdict {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// Kleene `or`: true if either side is true, false if both are false.
fn verdict_or(a: Verdict, b: Verdict) -> Verdict {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// `or_expr := and_expr ('or' and_expr)*`: the verdict and the tokens used.
/// `None` when the tokens do not parse.
fn marker_or(toks: &[MarkerTok<'_>]) -> Option<(Verdict, usize)> {
    let (mut acc, mut used) = marker_and(toks)?;
    while toks.get(used) == Some(&MarkerTok::Word("or")) {
        let (v, n) = marker_and(&toks[used + 1..])?;
        acc = verdict_or(acc, v);
        used += 1 + n;
    }
    Some((acc, used))
}

/// `and_expr := not_expr ('and' not_expr)*`.
fn marker_and(toks: &[MarkerTok<'_>]) -> Option<(Verdict, usize)> {
    let (mut acc, mut used) = marker_not(toks)?;
    while toks.get(used) == Some(&MarkerTok::Word("and")) {
        let (v, n) = marker_not(&toks[used + 1..])?;
        acc = verdict_and(acc, v);
        used += 1 + n;
    }
    Some((acc, used))
}

/// `not_expr := 'not' not_expr | atom`. `not` of an unknown verdict is unknown.
fn marker_not(toks: &[MarkerTok<'_>]) -> Option<(Verdict, usize)> {
    if toks.first() == Some(&MarkerTok::Word("not")) {
        let (v, n) = marker_not(&toks[1..])?;
        return Some((v.map(|b| !b), n + 1));
    }
    marker_atom(toks)
}

/// `atom := '(' or_expr ')' | clause`.
fn marker_atom(toks: &[MarkerTok<'_>]) -> Option<(Verdict, usize)> {
    match toks.first()? {
        MarkerTok::Open => {
            let (v, n) = marker_or(&toks[1..])?;
            (toks.get(n + 1) == Some(&MarkerTok::Close)).then_some((v, n + 2))
        }
        MarkerTok::Close | MarkerTok::Word("and" | "or" | "in") => None,
        _ => marker_clause_verdict(toks),
    }
}

/// Whether a marker is false for the build target (Linux, CPython), evaluated
/// with three-valued logic: a clause the target cannot decide (`python_version`,
/// `extra`, ...) is unknown. `not` binds tighter than `and`, which binds tighter
/// than `or`, and parentheses group. Only a marker that is definitely false is
/// excluded; a true or unknown one, and one that does not parse (unbalanced
/// parentheses included), is kept.
fn marker_excludes_target(marker: &str) -> bool {
    let Some(toks) = tokenize_marker(marker) else {
        return false;
    };
    match marker_or(&toks) {
        Some((verdict, used)) if used == toks.len() => verdict == Some(false),
        _ => false,
    }
}

/// Whether a PEP 508 marker is made only of what a marker needs: letters
/// (marker names and the `and`/`or`/`in`/`not` operators), digits, spaces,
/// `. _ - + * ! = < > ~ ( )` and quotes. Newline, `%`, `{`, `}`, `$`, backtick,
/// `;`, `|`, `&`, backslash and `#` are refused.
fn is_safe_marker(marker: &str) -> bool {
    marker.len() <= 300
        && marker.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    ' ' | '.'
                        | '_'
                        | '-'
                        | '+'
                        | '*'
                        | '!'
                        | '='
                        | '<'
                        | '>'
                        | '~'
                        | '('
                        | ')'
                        | '\''
                        | '"'
                )
        })
}
