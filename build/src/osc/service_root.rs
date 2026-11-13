//! Whether a `_service` file has the `<services>` root that the services are read
//! from. An empty file, or one with only comments, has none: the service run then runs
//! no service, and an edit that leaves one is refused.

use ap_core::event::Event;

use crate::proc::Emit;

/// Whether `xml` opens with a `<services>` element (`<services/>` counts). What comes
/// before it is the XML declaration, comments and white space only.
pub(crate) fn has_services_root(xml: &str) -> bool {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => return e.name().as_ref() == "services",
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
    }
}

/// Warns when the `_service` of `package` has no `<services>` root: the service run
/// then runs no service, and the run goes on.
pub(crate) fn warn_if_no_root(xml: &[u8], package: &str, emit: Emit<'_>) {
    if !has_services_root(&String::from_utf8_lossy(xml)) {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!(
                "the _service of {package} has no <services> element: no service runs"
            ),
        });
    }
}
