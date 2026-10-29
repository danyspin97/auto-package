use super::*;

/// An answer from a background thread.
pub enum Job {
    Diff {
        row: String,
        /// Which request this answers, so a retry's older answer is dropped.
        seq: u64,
        result: Result<String, String>,
    },
    Explain {
        row: String,
        seq: u64,
        result: Result<Explanation, String>,
    },
    Preview(Vec<SubmitEntry>),
    Submitted {
        row: String,
        result: Result<String, SubmitFailure>,
    },
}

/// Fold an answer into the state.
pub fn apply_job(state: &mut TuiState, browse: &mut BrowseState, job: Job) {
    match job {
        Job::Diff { row, seq, result } => {
            if let Some(Modal::Diff(v)) = &mut browse.modal
                && v.row == row
                && v.seq == seq
            {
                v.load = match result {
                    Ok(text) => DiffLoad::Ready(diffview::parse(&text)),
                    Err(e) => DiffLoad::Failed(e),
                };
                *v.rendered.borrow_mut() = None;
                v.scroll.set(0);
            }
        }
        Job::Explain { row, seq, result } => {
            if let Some(Modal::Explain(v)) = &mut browse.modal
                && v.row == row
                && v.seq == seq
            {
                v.load = match result {
                    Ok(ex) => ExplainLoad::Ready(Box::new(ex)),
                    Err(e) => ExplainLoad::Failed(e),
                };
                *v.rendered.borrow_mut() = None;
                v.scroll.set(0);
            }
        }
        Job::Preview(entries) => {
            for e in &entries {
                if let Some(id) = &e.existing
                    && let Some(p) = state.packages.get_mut(&e.row)
                {
                    p.sr = Some(SrMark::Existing(id.clone()));
                }
            }
            if let Some(Modal::Submit(m)) = &mut browse.modal
                && matches!(m.stage, SubmitStage::Loading(_))
            {
                m.stage = SubmitStage::Ready(entries);
            }
        }
        Job::Submitted { row, result } => {
            let (mark, level, message) = match result {
                Ok(id) => (
                    SrMark::Created(id.clone()),
                    LogLevel::Success,
                    format!("Submit request #{id} created"),
                ),
                Err(SubmitFailure::Existing(id)) => (
                    SrMark::Existing(id.clone()),
                    LogLevel::Warn,
                    format!("Not submitted: request #{id} is already open"),
                ),
                Err(SubmitFailure::Other(why)) => (
                    SrMark::Failed(why.clone()),
                    LogLevel::Error,
                    format!("Submit failed: {why}"),
                ),
            };
            if let Some(p) = state.packages.get_mut(&row) {
                p.sr = Some(mark);
                p.add_log(level, message.clone());
            }
            state.add_global_log(level, format!("{row}: {message}"));
        }
    }
}
