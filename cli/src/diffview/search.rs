/// The first of `starts` after line `cur`.
pub fn next_start(starts: &[usize], cur: usize) -> Option<usize> {
    starts.iter().copied().find(|&s| s > cur)
}

/// The last of `starts` before line `cur`.
pub fn prev_start(starts: &[usize], cur: usize) -> Option<usize> {
    starts.iter().rev().copied().find(|&s| s < cur)
}

/// The next line (after `from` when `forward`, before it otherwise) whose text
/// holds `needle`, ignoring case; wraps around.
pub fn find_line(plain: &[String], needle: &str, from: usize, forward: bool) -> Option<usize> {
    if needle.is_empty() || plain.is_empty() {
        return None;
    }
    let needle = needle.to_lowercase();
    let n = plain.len();
    (1..=n)
        .map(|step| {
            if forward {
                (from + step) % n
            } else {
                (from + n - step % n) % n
            }
        })
        .find(|&i| plain[i].to_lowercase().contains(&needle))
}
