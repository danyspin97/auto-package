//! The full detail of a failed package as the report prints it: each line
//! keeps its own spacing and column, indented under the reason. Only a line
//! longer than the width is cut, at the width, so nothing is reflowed.

const INDENT: &str = "    ";

/// The detail as lines for a report `width` columns wide. Blank lines stay
/// blank; a line that does not fit is continued on the next line.
pub(super) fn verbatim_lines(full: &str, width: usize) -> Vec<String> {
    let avail = width.saturating_sub(INDENT.len()).max(20);
    let mut out = Vec::new();
    for line in full.lines() {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for chunk in chars.chunks(avail) {
            out.push(format!("{INDENT}{}", chunk.iter().collect::<String>()));
        }
    }
    out
}
