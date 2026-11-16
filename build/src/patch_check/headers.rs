//! The file prefixes of a refreshed patch's headers. quilt names the files after
//! the directory it runs in: in the workspace that is `source/` (and
//! `source.orig/` for the original), not the archive's top directory, which is
//! what `%autosetup -n` and the sdist root name. Only those two prefixes change,
//! and only in `Index:`, `---` and `+++` header lines (a removed line `-- x` in a
//! hunk reads as `--- x`, so hunk bodies are counted, not matched).

/// `text` with the `source/` and `source.orig/` prefixes of its header paths
/// made `<top>/` and `<top>.orig/`. `None` when nothing changes: no such prefix,
/// a patch with `a/` or `b/` paths, or a `top` that is not a usable directory
/// name. Applying it to its own output changes nothing.
pub fn rewrite_header_prefixes(bytes: &[u8], top: &str) -> Option<Vec<u8>> {
    if !usable_top(top) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let headers: Vec<(usize, &str, &str, &str)> = header_lines(&lines)
        .into_iter()
        .filter_map(|i| split_header(lines[i]).map(|(marker, path, tail)| (i, marker, path, tail)))
        .collect();
    if headers
        .iter()
        .any(|(_, _, path, _)| path.starts_with("a/") || path.starts_with("b/"))
    {
        return None;
    }
    let replaced: std::collections::BTreeMap<usize, String> = headers
        .iter()
        .filter_map(|&(i, marker, path, tail)| {
            retarget(path, top).map(|new| (i, format!("{marker}{new}{tail}")))
        })
        .collect();
    if replaced.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        match replaced.get(&i) {
            Some(new) => out.push_str(new),
            None => out.push_str(line),
        }
    }
    Some(out.into_bytes())
}

/// The archive's top directory as a prefix: a usable name that is not one of
/// the prefixes this rewrite reads or writes. Only the exact names matter:
/// `source-highlight-3.1` writes no `source/` prefix, so it is usable.
fn usable_top(top: &str) -> bool {
    ap_ecosystem::sdist::is_safe_top_level_dir(top)
        && !matches!(top, "source" | "source.orig" | "a" | "b")
}

/// The new path of a header path with the `source` prefix, `None` otherwise.
fn retarget(path: &str, top: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("source/") {
        return Some(format!("{top}/{rest}"));
    }
    path.strip_prefix("source.orig/")
        .map(|rest| format!("{top}.orig/{rest}"))
}

/// The marker, the path and the rest (a tab and a timestamp, the line ending)
/// of a header line.
fn split_header(line: &str) -> Option<(&str, &str, &str)> {
    let marker = ["Index: ", "--- ", "+++ "]
        .into_iter()
        .find(|m| line.starts_with(*m))?;
    let rest = &line[marker.len()..];
    let end = rest.find(['\t', '\n', '\r']).unwrap_or(rest.len());
    Some((marker, &rest[..end], &rest[end..]))
}

/// The indexes of the header lines: `Index:` lines, and each `---` line that
/// a `+++` line follows. Hunk bodies are skipped by their `@@` line counts.
fn header_lines(lines: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let (mut old, mut new) = (0usize, 0usize);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if old > 0 || new > 0 {
            match line.as_bytes().first() {
                Some(b'\\') => {}
                Some(b'+') => new = new.saturating_sub(1),
                Some(b'-') => old = old.saturating_sub(1),
                _ => {
                    old = old.saturating_sub(1);
                    new = new.saturating_sub(1);
                }
            }
        } else if let Some((o, n)) = hunk_counts(line) {
            (old, new) = (o, n);
        } else if line.starts_with("Index: ") {
            out.push(i);
        } else if line.starts_with("--- ")
            && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "))
        {
            out.push(i);
            out.push(i + 1);
            i += 2;
            continue;
        }
        i += 1;
    }
    out
}

/// The old and new line counts of a `@@ -a[,b] +c[,d] @@` line.
fn hunk_counts(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.strip_prefix("@@ ")?.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((range_count(old)?, range_count(new)?))
}

fn range_count(range: &str) -> Option<usize> {
    match range.split_once(',') {
        Some((_, n)) => n.parse().ok(),
        None => Some(1),
    }
}
