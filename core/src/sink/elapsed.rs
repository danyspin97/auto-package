use std::time::Duration;

/// A duration for people: `850ms`, `12.3s`, `4m07s`, `1h02m`.
pub fn fmt_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        0..1000 => format!("{ms}ms"),
        1000..60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        60_000..3_600_000 => format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60),
        _ => format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60),
    }
}
