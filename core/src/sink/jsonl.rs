use super::*;

/// The open file and when it was last flushed.
struct JsonlWriter {
    out: BufWriter<File>,
    last_flush: Instant,
}

/// Longest the tail of run.jsonl may sit in the buffer while events keep
/// arriving.
const JSONL_FLUSH_EVERY: Duration = Duration::from_secs(1);

/// Every open [`JsonlSink`]'s writer, so [`flush_jsonl_sinks`] can reach them
/// from a path that ends the process without dropping them.
static JSONL_WRITERS: Mutex<Vec<Weak<Mutex<JsonlWriter>>>> = Mutex::new(Vec::new());

/// Flush every open run.jsonl. Call before `process::exit`, which skips the
/// buffers' drop: events are only flushed on a timer and on terminal events.
pub fn flush_jsonl_sinks() {
    let writers = JSONL_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    for w in writers.iter().filter_map(Weak::upgrade) {
        let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.out.flush();
        w.last_flush = Instant::now();
    }
}

/// Start, once per process, the thread that flushes every run.jsonl each
/// [`JSONL_FLUSH_EVERY`]. The per-event timer only fires when another event
/// arrives, so without this a quiet stretch (a long build wait) leaves the
/// tail buffered, and a SIGKILL or panic loses it. The thread is detached and
/// sleeps between rounds, so it neither keeps the process alive nor delays
/// its exit.
fn start_jsonl_flusher() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("jsonl-flush".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(JSONL_FLUSH_EVERY);
                    flush_jsonl_sinks();
                }
            });
        // Without it the timer-on-event flush still applies.
        drop(spawned);
    });
}

pub struct JsonlSink {
    writer: Arc<Mutex<JsonlWriter>>,
    /// Set after the first write failure so the diagnostic prints once, not
    /// once per event.
    pub(super) warned: AtomicBool,
    /// Likewise for an event that could not be serialized.
    serialize_warned: AtomicBool,
}

impl JsonlSink {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        let writer = Arc::new(Mutex::new(JsonlWriter {
            out: BufWriter::new(file),
            last_flush: Instant::now(),
        }));
        let mut writers = JSONL_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
        writers.retain(|w| w.strong_count() > 0);
        writers.push(Arc::downgrade(&writer));
        drop(writers);
        start_jsonl_flusher();
        Ok(Self {
            writer,
            warned: AtomicBool::new(false),
            serialize_warned: AtomicBool::new(false),
        })
    }
}

/// JSONL line format. Wrapping the event in an envelope lets us carry
/// per-package attribution — set by the ambient [`PkgContextReporter`] —
/// alongside the event without touching the `Event` enum's variants (and
/// therefore without having to update every one of its ~124 construction
/// sites across the workspace, several of which live in files owned by
/// other parallel agents).
#[derive(Serialize)]
struct JsonlEnvelope<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pkg: Option<String>,
    #[serde(flatten)]
    event: &'a Event,
}

/// `event` with credentials removed from the commands and the agent stream.
/// Borrowed when it carries none of those.
fn redacted_event(event: &Event) -> std::borrow::Cow<'_, Event> {
    use crate::redact::redact;
    let mut e = event.clone();
    match &mut e {
        Event::Command { argv, .. } => argv.iter_mut().for_each(|a| *a = redact(a)),
        Event::AgentToolUse { input, .. } => *input = redact(input),
        Event::AgentPrompt { prompt, .. } => *prompt = redact(prompt),
        Event::AgentReply { raw, .. } => *raw = redact(raw),
        _ => return std::borrow::Cow::Borrowed(event),
    }
    std::borrow::Cow::Owned(e)
}

impl Reporter for JsonlSink {
    fn report(&self, event: &Event) {
        let event = redacted_event(event);
        let event: &Event = &event;
        let envelope = JsonlEnvelope {
            pkg: current_pkg(),
            event,
        };
        let line = match serde_json::to_string(&envelope) {
            Ok(line) => line,
            Err(e) => {
                // The event is missing from the log, so say so, once.
                if !self.serialize_warned.swap(true, Ordering::Relaxed) {
                    diag!(
                        "warning: run.jsonl could not record an event ({e}); events may be missing"
                    );
                }
                return;
            }
        };
        // A panic in another thread mid-write poisons the lock; the
        // buffer is still usable, so keep logging.
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        // Flushed on events that close something out and at most a second
        // apart otherwise, not per event. The CLI's exit paths call
        // `process::exit`, which skips the BufWriter's drop, so they call
        // [`flush_jsonl_sinks`] first.
        let terminal = matches!(
            event,
            Event::PackageFinished { .. } | Event::RunFinished { .. } | Event::Warning { .. }
        );
        let due = terminal || w.last_flush.elapsed() >= JSONL_FLUSH_EVERY;
        let res = writeln!(w.out, "{line}").and_then(|()| {
            if due {
                w.last_flush = Instant::now();
                w.out.flush()
            } else {
                Ok(())
            }
        });
        if let Err(e) = res
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            diag!("warning: run.jsonl write failed ({e}); events may be missing");
        }
    }
}
