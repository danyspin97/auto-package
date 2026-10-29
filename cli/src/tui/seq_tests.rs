
use super::*;

#[test]
fn log_seq_keeps_counting_after_eviction_caps_the_log() {
    let mut pkg = PkgState::new(String::new());
    for i in 0..MAX_LOG_LINES + 5 {
        pkg.add_log(LogLevel::Info, i.to_string());
    }
    assert_eq!(pkg.log_lines.len(), MAX_LOG_LINES);
    assert_eq!(pkg.log_seq, (MAX_LOG_LINES + 5) as u64);
}

#[test]
fn activity_seq_keeps_counting_after_eviction_caps_the_list() {
    let mut pkg = PkgState::new(String::new());
    for i in 0..MAX_AGENT_ACTIVITY + 3 {
        pkg.add_agent_activity(ActivityKind::Task, i.to_string());
    }
    assert_eq!(pkg.agent_activity.len(), MAX_AGENT_ACTIVITY);
    assert_eq!(pkg.activity_seq, (MAX_AGENT_ACTIVITY + 3) as u64);
}
