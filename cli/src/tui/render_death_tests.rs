
use super::RenderDeath;

#[test]
fn a_live_render_loop_never_switches_to_plain_output() {
    let death = RenderDeath::default();
    assert!(!death.should_switch());
    assert!(!death.should_switch());
}

#[test]
fn the_switch_happens_once_after_the_render_loop_dies() {
    let death = RenderDeath::default();
    death.mark_died();
    assert!(death.should_switch());
    assert!(!death.should_switch());
    assert!(!death.should_switch());
}

#[test]
fn repeated_deaths_still_switch_once() {
    let death = RenderDeath::default();
    death.mark_died();
    death.mark_died();
    assert!(death.should_switch());
    assert!(!death.should_switch());
}
