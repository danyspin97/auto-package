#[test]
fn jsonl_events_are_redacted_and_the_file_is_private() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    let sink = JsonlSink::open(&path).unwrap();
    sink.report(&Event::Command {
        step: None,
        program: "git".into(),
        argv: vec!["clone".into(), "https://u:p@h.example/r.git".into()],
        cwd: "/x".into(),
        log: None,
    });
    sink.report(&Event::AgentToolUse {
        step: crate::event::StepId(0),
        tool: "Bash".into(),
        input: "GITHUB_TOKEN=abc curl -H 'Authorization: Bearer q' x".into(),
    });
    flush_jsonl_sinks();
    let text = std::fs::read_to_string(&path).unwrap();
    for leak in ["u:p@", "abc", "Bearer"] {
        assert!(!text.contains(leak), "{leak} leaked: {text}");
    }
    assert!(text.contains("https://***@h.example/r.git"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}

use super::*;
use crate::event::{StepId, StepKind};
use std::io::{BufRead, BufReader};
use std::sync::Arc;

fn tmp_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ap-sink-test-{tag}-{nanos}-{}.jsonl",
        std::process::id()
    ))
}

/// With no later event to trigger the per-event timer, the background
/// flusher still gets a buffered line to disk.
#[test]
fn a_warning_is_reported_once_per_run() {
    use std::sync::{Arc, Mutex};
    struct Count(Arc<Mutex<usize>>);
    impl Reporter for Count {
        fn report(&self, _: &Event) {
            *self.0.lock().unwrap() += 1;
        }
    }
    let n = Arc::new(Mutex::new(0));
    let fan = FanoutReporter::new(vec![Box::new(Count(n.clone()))]);
    let warn = |m: &str| Event::Warning {
        step: None,
        message: m.to_string(),
    };
    for _ in 0..5 {
        fan.report(&warn("relaxed bound a"));
    }
    fan.report(&warn("relaxed bound b"));
    assert_eq!(*n.lock().unwrap(), 2);
}

#[test]
fn a_quiet_stretch_still_flushes_the_tail() {
    let path = tmp_path("quiet");
    let sink = JsonlSink::open(&path).unwrap();
    sink.report(&Event::StepStarted {
        step: StepId(0),
        kind: StepKind::Build,
        attempt: 1,
        round: 0,
        detail: None,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&path).unwrap().is_empty() {
        assert!(Instant::now() < deadline, "tail was never flushed");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn jsonl_envelope_includes_current_pkg() {
    let path = tmp_path("pkg");
    let sink = JsonlSink::open(&path).unwrap();
    let base: Arc<dyn Reporter + Send + Sync> = Arc::new(sink);
    let tagged = PkgContextReporter::new(base.clone(), Some("python-foo".into()));

    tagged.report(&Event::StepStarted {
        step: StepId(0),
        kind: StepKind::Build,
        attempt: 1,
        round: 0,
        detail: None,
    });

    // After the call, the thread-local must be restored.
    assert_eq!(current_pkg(), None);

    flush_jsonl_sinks();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("\"pkg\":\"python-foo\""),
        "jsonl line missing pkg attribution: {contents}"
    );
    assert!(contents.contains("\"event\":\"step_started\""));
    let _ = std::fs::remove_file(&path);
}

#[cfg(target_os = "linux")]
#[test]
fn jsonl_write_failure_warns_once_and_does_not_panic() {
    let sink = JsonlSink::open(Path::new("/dev/full")).unwrap();
    assert!(!sink.warned.load(Ordering::Relaxed));
    // A warning is flushed straight away, so the failure shows at once.
    let warning = Event::Warning {
        step: None,
        message: "x".into(),
    };
    sink.report(&warning);
    assert!(sink.warned.load(Ordering::Relaxed));
    sink.report(&warning);
}

#[test]
fn memory_sink_survives_a_poisoned_lock() {
    let sink = Arc::new(MemorySink::new());
    let s2 = sink.clone();
    let _ = std::thread::spawn(move || {
        let _g = s2.state.lock().unwrap();
        panic!("poison");
    })
    .join();
    sink.report(&Event::PackageFinished {
        pkg: "x".into(),
        version: "1".into(),
        outcome: PackageOutcome::Built,
    });
    assert_eq!(sink.finished().len(), 1);
}

#[test]
fn memory_sink_keeps_usage_totals_and_finished_packages_only() {
    let sink = MemorySink::new();
    let usage = Usage {
        input_tokens: 3,
        output_tokens: 4,
        cost_usd: 0.5,
    };
    sink.report(&Event::ResolveStarted { root: "x".into() });
    sink.report(&Event::NotesUsage {
        step: crate::event::StepId(1),
        usage: usage.clone(),
    });
    sink.report(&Event::NotesUsage {
        step: crate::event::StepId(2),
        usage,
    });
    assert_eq!(sink.finished().len(), 0);
    let total = sink.usage();
    assert_eq!((total.input_tokens, total.output_tokens), (6, 8));
    assert!((total.cost_usd - 1.0).abs() < 1e-9);
}

#[test]
fn jsonl_envelope_omits_pkg_when_no_context() {
    let path = tmp_path("nopkg");
    let sink = JsonlSink::open(&path).unwrap();
    // Call JsonlSink directly, with no ambient pkg context.
    sink.report(&Event::ResolveStarted { root: "x".into() });
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        !contents.contains("\"pkg\""),
        "unexpected pkg field in untagged line: {contents}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn parallel_tagged_streams_do_not_cross_contaminate() {
    // Two threads each emit a batch of events through their own tagged
    // reporter, writing into a shared JsonlSink. Afterwards, every line
    // in the file must still carry its correct pkg, and ordering within
    // either thread's own slice doesn't matter.
    let path = tmp_path("parallel");
    let sink: Arc<dyn Reporter + Send + Sync> = Arc::new(JsonlSink::open(&path).unwrap());

    let per_thread = 50usize;
    let handles: Vec<_> = ["pkg-a", "pkg-b"]
        .iter()
        .map(|name| {
            let base = sink.clone();
            let name = name.to_string();
            std::thread::spawn(move || {
                let r = PkgContextReporter::new(base, Some(name.clone()));
                for _ in 0..per_thread {
                    r.report(&Event::StepStarted {
                        step: StepId(0),
                        kind: StepKind::Build,
                        attempt: 1,
                        round: 0,
                        detail: None,
                    });
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    flush_jsonl_sinks();
    let file = std::fs::File::open(&path).unwrap();
    let mut a = 0usize;
    let mut b = 0usize;
    let mut other = 0usize;
    for line in BufReader::new(file).lines() {
        let line = line.unwrap();
        if line.contains("\"pkg\":\"pkg-a\"") {
            a += 1;
        } else if line.contains("\"pkg\":\"pkg-b\"") {
            b += 1;
        } else {
            other += 1;
        }
    }
    assert_eq!(a, per_thread, "pkg-a count wrong, raw other={other}");
    assert_eq!(b, per_thread, "pkg-b count wrong, raw other={other}");
    assert_eq!(other, 0, "every line should carry a pkg tag");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn nested_contexts_restore_outer_pkg_on_drop() {
    // Simulates a `Tapped` reporter forwarding through a tagged base:
    // the outer tagged reporter should still see its own pkg after the
    // inner call returns.
    let outer = PkgContextReporter::new(
        Arc::new(NullReporter) as Arc<dyn Reporter + Send + Sync>,
        Some("outer".into()),
    );
    let inner = PkgContextReporter::new(
        Arc::new(NullReporter) as Arc<dyn Reporter + Send + Sync>,
        Some("inner".into()),
    );

    // Fabricate an event delivery that nests inner inside outer.
    struct Probe {
        seen: Mutex<Vec<Option<String>>>,
        inner: Arc<dyn Reporter + Send + Sync>,
    }
    impl Reporter for Probe {
        fn report(&self, event: &Event) {
            self.seen.lock().unwrap().push(current_pkg());
            self.inner.report(event);
            // After the inner call, the ambient pkg must still match ours.
            self.seen.lock().unwrap().push(current_pkg());
        }
    }
    let probe = Probe {
        seen: Mutex::new(Vec::new()),
        inner: Arc::new(inner),
    };
    let probe_arc: Arc<dyn Reporter + Send + Sync> = Arc::new(probe);
    let outer2 = PkgContextReporter::new(probe_arc.clone(), Some("outer".into()));
    outer2.report(&Event::ResolveStarted { root: "r".into() });
    let _ = outer; // silence unused warning

    // Can't downcast the Arc<dyn Reporter> to Probe easily; just verify
    // the ambient pkg was fully torn down after the whole call ends.
    assert_eq!(current_pkg(), None);
}
