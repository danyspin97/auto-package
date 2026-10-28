#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DLine {
    pub kind: Kind,
    pub text: String,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// The function/section part after the second `@@`.
    pub context: String,
    pub lines: Vec<DLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileDiff {
    pub old: String,
    pub new: String,
    pub hunks: Vec<Hunk>,
    /// Set for a file whose content is not shown: what to say instead.
    pub binary: Option<String>,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    /// The name shown for the file.
    pub fn title(&self) -> String {
        match (self.old.as_str(), self.new.as_str()) {
            ("/dev/null", n) => format!("{n} (new file)"),
            (o, "/dev/null") => format!("{o} (deleted)"),
            (o, n) if o == n || o.is_empty() => n.to_string(),
            (o, "") => o.to_string(),
            (o, n) => format!("{o} \u{2192} {n}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// Input lines past [`MAX_DIFF_LINES`] that were not read.
    pub dropped_lines: usize,
}

impl Diff {
    pub fn added(&self) -> usize {
        self.files.iter().map(|f| f.added).sum()
    }

    pub fn removed(&self) -> usize {
        self.files.iter().map(|f| f.removed).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// One line: files changed, +N, -M.
    pub fn summary(&self) -> String {
        if self.files.is_empty() {
            return "no changes".to_string();
        }
        let n = self.files.len();
        let binary = self.files.iter().filter(|f| f.binary.is_some()).count();
        let mut s = format!(
            "{n} file{} changed, +{} \u{2212}{}",
            if n == 1 { "" } else { "s" },
            self.added(),
            self.removed()
        );
        if binary > 0 {
            s.push_str(&format!(", {binary} binary"));
        }
        if self.dropped_lines > 0 {
            s.push_str(", truncated");
        }
        s
    }
}
