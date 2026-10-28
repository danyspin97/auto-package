use super::*;

/// Split `s` into word, space and punctuation tokens, as byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<(usize, u8)> = None;
    let class = |c: char| -> u8 {
        if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            0
        }
    };
    for (i, c) in s.char_indices() {
        let k = class(c);
        match start {
            Some((_, sk)) if sk == k && k != 0 => {}
            Some((st, _)) => {
                out.push(st..i);
                start = Some((i, k));
            }
            None => start = Some((i, k)),
        }
    }
    if let Some((st, _)) = start {
        out.push(st..s.len());
    }
    out
}

/// The parts of `old` and `new` that differ, as byte ranges in each, found by
/// a longest-common-subsequence over word tokens. `None` when the lines are
/// equal, share no word, or are too long to compare.
pub fn word_diff(old: &str, new: &str) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    if old == new {
        return None;
    }
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MAX_WORD_TOKENS || b.len() > MAX_WORD_TOKENS {
        return None;
    }
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if old[a[i].clone()] == new[b[j].clone()] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut ca, mut cb) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    let mut shared_word = false;
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            ca[i] = true;
            cb[j] = true;
            if !old[a[i].clone()].trim().is_empty() {
                shared_word = true;
            }
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if !shared_word {
        return None;
    }
    let merge = |toks: &[Range<usize>], common: &[bool]| -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (tok, c) in toks.iter().zip(common) {
            if *c {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == tok.start => last.end = tok.end,
                _ => out.push(tok.clone()),
            }
        }
        out
    };
    Some((merge(&a, &ca), merge(&b, &cb)))
}
