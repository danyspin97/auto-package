use super::*;

#[test]
fn durations_read_at_their_own_scale() {
    assert_eq!(fmt_elapsed(Duration::from_millis(850)), "850ms");
    assert_eq!(fmt_elapsed(Duration::from_millis(12_340)), "12.3s");
    assert_eq!(fmt_elapsed(Duration::from_secs(247)), "4m07s");
    assert_eq!(fmt_elapsed(Duration::from_secs(3720)), "1h02m");
}

#[test]
fn no_duration_is_printed_as_a_bare_minute_fragment() {
    let cases = [
        (Duration::ZERO, "0ms"),
        (Duration::from_micros(1), "0ms"),
        (Duration::from_micros(999), "0ms"),
        (Duration::from_millis(1), "1ms"),
        (Duration::from_millis(59_900), "59.9s"),
        (Duration::from_secs(60), "1m00s"),
        (Duration::from_secs(61), "1m01s"),
        (Duration::from_secs(3600), "1h00m"),
    ];
    for (d, want) in cases {
        assert_eq!(fmt_elapsed(d), want, "for {d:?}");
    }
}
