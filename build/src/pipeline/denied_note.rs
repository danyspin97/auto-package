//! The warning for the commands a turn's sandbox refused.

use super::render::brief;

/// The line for `denied`, the commands one turn ran that the sandbox refused.
/// It says what the refusal means and what the run does about it; the commands
/// are named only when they are not package lookups. `lookup_granted` is
/// whether the turn had auto-package's own lookup call to ask instead.
pub(super) fn denied_commands_message(
    pkg: &str,
    denied: &[String],
    lookup_granted: bool,
) -> String {
    let lookups = denied
        .iter()
        .filter(|c| c.trim_start().starts_with("zypper "))
        .count();
    if lookups == denied.len() {
        let after = if lookup_granted {
            "the agent is told to ask auto-package's own lookup instead, so this only costs it \
             a retry"
        } else {
            "this turn had no auto-package lookup call, so the agent went without the lookup"
        };
        return format!(
            "{pkg}: {} package lookup(s) by zypper were refused by the agent's sandbox; {after}",
            denied.len()
        );
    }
    let others: Vec<String> = denied
        .iter()
        .filter(|c| !c.trim_start().starts_with("zypper "))
        .take(3)
        .map(|c| format!("`{}`", brief(c)))
        .collect();
    format!(
        "{pkg}: the agent's sandbox refused {} command(s) (it does not run them and the \
         agent carries on without): {}",
        denied.len(),
        others.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zypper_lookups_are_described_without_the_commands() {
        let m = denied_commands_message(
            "pleamar",
            &[
                "zypper search 'pkgconfig(x)'".into(),
                "zypper what-provides 'a(b)'".into(),
            ],
            true,
        );
        assert!(
            m.contains("2 package lookup(s)") && !m.contains("pkgconfig(x)"),
            "{m}"
        );
        assert!(m.contains("lookup instead"), "{m}");
    }

    #[test]
    fn other_commands_are_named_and_counted() {
        let m =
            denied_commands_message("p", &["zypper info x".into(), "curl http://x".into()], true);
        assert!(
            m.contains("refused 2 command(s)") && m.contains("`curl http://x`"),
            "{m}"
        );
        assert!(!m.contains("zypper info"), "{m}");
    }

    #[test]
    fn a_turn_without_the_lookup_is_not_told_to_use_it() {
        let m = denied_commands_message("p", &["zypper search x".into()], false);
        assert!(
            m.contains("no auto-package lookup call") && !m.contains("is told to ask"),
            "{m}"
        );
    }
}
