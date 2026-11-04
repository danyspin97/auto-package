//! Adding a patch to a spec without a model.

use crate::backport::spec_applies;
use crate::spec::parse_source_line;

/// The column the value of a `Tag:` line starts at: after the colon and its
/// padding.
fn value_column(line: &str) -> usize {
    let colon = line.find(':').map_or(0, |i| i + 1);
    colon + line[colon..].len() - line[colon..].trim_start().len()
}

/// `spec` with `comment` and a `PatchN: file` line after the last `PatchN:` line
/// (after the last `SourceN:` line when there is no patch), N one more than the
/// highest number the spec declares and the value aligned with the line it
/// follows. `Err` when the spec has no place for it, or does not apply the
/// patch by itself afterwards (it needs `%autosetup` or `%autopatch`: a spec
/// with `%patch` lines is for the agent).
pub fn spec_adding_patch(spec: &str, comment: &str, file: &str) -> Result<String, String> {
    let lines: Vec<&str> = spec.lines().collect();
    let tagged = |tags: &[&str]| -> Vec<(usize, u32)> {
        lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| parse_source_line(l, tags, str::to_string).map(|s| (i, s.index)))
            .collect()
    };
    let patches = tagged(&["patch"]);
    let after = patches
        .last()
        .or(tagged(&["source"]).last())
        .map(|(i, _)| *i)
        .ok_or("the spec has no Source: or Patch: line to put the patch after")?;
    let number = patches.iter().map(|(_, n)| n + 1).max().unwrap_or(0);
    let neighbour = lines[after];
    let tag = format!("Patch{number}:");
    let column = value_column(neighbour).max(tag.len() + 1);
    let new = format!("{tag:<column$}{file}");
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(after + 1, new);
    out.insert(after + 1, comment.to_string());
    let mut text = out.join("\n");
    if spec.ends_with('\n') {
        text.push('\n');
    }
    if !spec_applies(&text, file) {
        return Err(format!(
            "the spec does not apply {file} by itself: it has no %autosetup or %autopatch, so the \
             %patch line is the agent's to add"
        ));
    }
    Ok(text)
}
