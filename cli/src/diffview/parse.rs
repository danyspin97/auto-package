use super::*;

/// Whether a file name is an archive or other binary that is never shown.
fn is_binary_name(name: &str) -> bool {
    const EXT: &[&str] = &[
        ".tar", ".gz", ".tgz", ".bz2", ".xz", ".zst", ".zip", ".whl", ".jar", ".rpm", ".png",
        ".jpg", ".jpeg", ".gif", ".ico", ".pdf", ".7z", ".lz", ".so", ".bin",
    ];
    let lower = name.to_ascii_lowercase();
    EXT.iter().any(|e| lower.ends_with(e))
}

/// Expand tabs to tab stops and replace other control characters, then cap
/// the length.
pub fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    let mut chars = 0;
    for c in line.chars() {
        if chars >= MAX_LINE_CHARS {
            out.push('\u{2026}');
            break;
        }
        chars += 1;
        match c {
            '\t' => {
                let n = TAB_STOP - col % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' => {}
            c if c.is_control() => {
                out.push('\u{b7}');
                col += 1;
            }
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// A file name from a `---`/`+++` header: without the timestamp or revision
/// note after it and without the `a/` `b/` prefix.
fn header_name(rest: &str, prefix: &str) -> String {
    let rest = rest.split('\t').next().unwrap_or(rest);
    let rest = rest.split(" (").next().unwrap_or(rest).trim();
    rest.strip_prefix(prefix).unwrap_or(rest).to_string()
}

/// `@@ -a,b +c,d @@ context`.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, context) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        context: context.trim().to_string(),
        lines: Vec::new(),
    })
}

/// The file a `Binary file ... changed` line names.
fn binary_line_name(line: &str) -> String {
    for q in ['\'', '"'] {
        if let Some(a) = line.find(q)
            && let Some(len) = line[a + 1..].find(q)
        {
            return line[a + 1..a + 1 + len].to_string();
        }
    }
    // `Binary files a and b differ`
    let rest = line.trim_start_matches("Binary files ");
    let rest = rest.strip_suffix(" differ").unwrap_or(rest);
    let name = rest.rsplit_once(" and ").map_or(rest, |(_, b)| b);
    name.strip_prefix("b/").unwrap_or(name).trim().to_string()
}

/// Parse unified diff text. Lenient: lines it does not know (the section
/// banners `osc rdiff` prints, `Index:` lines) are skipped, and a diff
/// without any file header still parses as one anonymous file.
pub fn parse(text: &str) -> Diff {
    let mut diff = Diff::default();
    let mut file: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    let (mut rem_old, mut rem_new) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);

    fn close_hunk(file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        if let Some(h) = hunk.take()
            && let Some(f) = file.as_mut()
        {
            f.hunks.push(h);
        }
    }
    fn close_file(diff: &mut Diff, file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        close_hunk(file, hunk);
        if let Some(mut f) = file.take() {
            let name = if f.new.is_empty() || f.new == "/dev/null" {
                f.old.clone()
            } else {
                f.new.clone()
            };
            if f.binary.is_none() && !f.hunks.is_empty() && is_binary_name(&name) {
                let bytes: usize = f
                    .hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .map(|l| l.text.len() + 1)
                    .sum();
                f.binary = Some(format!(
                    "Binary file {name} changed ({bytes} bytes of diff not shown)"
                ));
            }
            if f.binary.is_some() {
                f.hunks.clear();
                f.added = 0;
                f.removed = 0;
            }
            diff.files.push(f);
        }
    }

    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    for (n, &raw) in all.iter().enumerate() {
        if n >= MAX_DIFF_LINES {
            diff.dropped_lines = total - n;
            break;
        }
        let in_hunk = hunk.is_some() && (rem_old > 0 || rem_new > 0);
        if in_hunk {
            let (kind, body) = match raw.chars().next() {
                Some(' ') => (Some(Kind::Context), &raw[1..]),
                Some('-') => (Some(Kind::Del), &raw[1..]),
                Some('+') => (Some(Kind::Add), &raw[1..]),
                None => (Some(Kind::Context), ""),
                Some('\\') => continue,
                _ => (None, raw),
            };
            if let Some(kind) = kind {
                let (o, nw) = match kind {
                    Kind::Context => {
                        rem_old = rem_old.saturating_sub(1);
                        rem_new = rem_new.saturating_sub(1);
                        old_no += 1;
                        new_no += 1;
                        (Some(old_no - 1), Some(new_no - 1))
                    }
                    Kind::Del => {
                        rem_old = rem_old.saturating_sub(1);
                        old_no += 1;
                        if let Some(f) = file.as_mut() {
                            f.removed += 1;
                        }
                        (Some(old_no - 1), None)
                    }
                    Kind::Add => {
                        rem_new = rem_new.saturating_sub(1);
                        new_no += 1;
                        if let Some(f) = file.as_mut() {
                            f.added += 1;
                        }
                        (None, Some(new_no - 1))
                    }
                };
                if let Some(h) = hunk.as_mut() {
                    h.lines.push(DLine {
                        kind,
                        text: clean(body),
                        old_no: o,
                        new_no: nw,
                    });
                }
                continue;
            }
            // Not a hunk line: the hunk ended early; read it as a header.
        }
        if let Some(rest) = raw.strip_prefix("--- ")
            && all.get(n + 1).is_some_and(|next| next.starts_with("+++ "))
        {
            close_file(&mut diff, &mut file, &mut hunk);
            file = Some(FileDiff {
                old: header_name(rest, "a/"),
                ..FileDiff::default()
            });
            continue;
        }
        if let Some(rest) = raw.strip_prefix("+++ ") {
            if let Some(f) = file.as_mut()
                && f.new.is_empty()
                && f.hunks.is_empty()
                && hunk.is_none()
            {
                f.new = header_name(rest, "b/");
            }
            continue;
        }
        if raw.starts_with("@@ ") {
            if let Some(h) = parse_hunk_header(raw) {
                close_hunk(&mut file, &mut hunk);
                file.get_or_insert_with(|| FileDiff {
                    old: String::new(),
                    new: "(diff)".to_string(),
                    ..FileDiff::default()
                });
                (rem_old, rem_new) = (h.old_len, h.new_len);
                (old_no, new_no) = (h.old_start, h.new_start);
                hunk = Some(h);
            }
            continue;
        }
        if raw.starts_with("Binary file") {
            let name = binary_line_name(raw);
            let note = format!(
                "Binary file {name} {}",
                if raw.contains("added") {
                    "added"
                } else if raw.contains("deleted") || raw.contains("removed") {
                    "deleted"
                } else {
                    "changed"
                }
            );
            let same = file
                .as_ref()
                .is_some_and(|f| f.hunks.is_empty() && (f.new == name || f.old == name));
            if !same {
                close_file(&mut diff, &mut file, &mut hunk);
                file = Some(FileDiff {
                    old: name.clone(),
                    new: name,
                    ..FileDiff::default()
                });
            }
            if let Some(f) = file.as_mut() {
                f.binary = Some(note);
            }
            close_file(&mut diff, &mut file, &mut hunk);
        }
        // Anything else (`Index:`, `====`, `changes files:`, `old:`, rules) is
        // not part of a file's content.
    }
    close_file(&mut diff, &mut file, &mut hunk);
    diff
}

/// A diff from the one-line-per-change text the pipeline records for a spec
/// (`- old` / `+ new`, no hunks or line numbers).
pub fn parse_recorded(name: &str, text: &str) -> Diff {
    let mut file = FileDiff {
        old: name.to_string(),
        new: name.to_string(),
        ..FileDiff::default()
    };
    let mut hunk = Hunk {
        context: "recorded changes".to_string(),
        ..Hunk::default()
    };
    for raw in text.lines() {
        let (kind, body) = if let Some(b) = raw.strip_prefix("- ") {
            (Kind::Del, b)
        } else if let Some(b) = raw.strip_prefix("+ ") {
            (Kind::Add, b)
        } else {
            continue;
        };
        match kind {
            Kind::Del => file.removed += 1,
            _ => file.added += 1,
        }
        hunk.lines.push(DLine {
            kind,
            text: clean(body),
            old_no: None,
            new_no: None,
        });
        if hunk.lines.len() >= MAX_DIFF_LINES {
            break;
        }
    }
    let mut diff = Diff::default();
    if !hunk.lines.is_empty() {
        file.hunks.push(hunk);
        diff.files.push(file);
    }
    diff
}

/// Merge several recorded diffs (one per spec change of a run) into one.
pub fn parse_recorded_all(name: &str, diffs: &[String]) -> Diff {
    let mut all = Diff::default();
    for d in diffs {
        all.files.extend(parse_recorded(name, d).files);
    }
    all
}
