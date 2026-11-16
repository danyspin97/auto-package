use std::sync::Mutex;

use ap_core::event::Event;
use ap_core::sink::Reporter;

use super::shutdown::report_unless_stopped;

#[derive(Default)]
struct Collect(Mutex<Vec<Event>>);

impl Reporter for Collect {
    fn report(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn warning(message: &str) -> Event {
    Event::Warning {
        step: None,
        message: message.into(),
    }
}

#[test]
fn an_osc_failure_is_warned_about_when_the_user_did_not_stop_the_run() {
    let collect = Collect::default();
    report_unless_stopped(false, &collect, &warning("could not fetch the log"));
    assert_eq!(collect.0.lock().unwrap().len(), 1);
}

#[test]
fn an_osc_failure_the_users_stop_caused_is_not_warned_about() {
    let collect = Collect::default();
    report_unless_stopped(true, &collect, &warning("could not fetch the log"));
    assert!(collect.0.lock().unwrap().is_empty());
}
