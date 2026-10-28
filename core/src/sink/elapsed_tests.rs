use super::*;

#[test]
fn durations_read_at_their_own_scale() {
    assert_eq!(fmt_elapsed(Duration::from_millis(850)), "850ms");
    assert_eq!(fmt_elapsed(Duration::from_millis(12_340)), "12.3s");
    assert_eq!(fmt_elapsed(Duration::from_secs(247)), "4m07s");
    assert_eq!(fmt_elapsed(Duration::from_secs(3720)), "1h02m");
}
