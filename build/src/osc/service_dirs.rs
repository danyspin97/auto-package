//! Directories a service run leaves at the top of the checkout. `cargo_vendor`
//! with `src=<dir>` works on the unpacked source tree `<dir>/`, and the outputs
//! of a run are moved into the checkout beside the spec. OBS never tracks such a
//! directory and the publish plan never publishes one, so without this it stays
//! untracked, and the commit warns about it on every run.

use super::*;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// The `_service` params that name a source directory of the checkout.
const SOURCE_DIR_PARAMS: &[&str] = &["src", "srcdir"];

/// The directory names the `_service` text gives as a source directory param,
/// trimmed. A value with a character or entity reference is left out: it is
/// not read as the name it would decode to.
pub(crate) fn service_source_dirs(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut dirs = Vec::new();
    // The value so far of the source-dir `<param>` being read; `None` elsewhere.
    let mut value: Option<String> = None;
    let mut plain = true;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "param" => {
                value = param_name(&e)
                    .filter(|n| SOURCE_DIR_PARAMS.contains(&n.as_str()))
                    .map(|_| String::new());
                plain = true;
            }
            Ok(Event::Text(t)) => {
                if let Some(v) = value.as_mut() {
                    v.push_str(&t);
                }
            }
            Ok(Event::GeneralRef(_)) => plain = false,
            Ok(Event::End(e)) if e.name().as_ref() == "param" => {
                if let Some(v) = value.take()
                    && plain
                {
                    let v = v.trim();
                    if !v.is_empty() {
                        dirs.push(v.to_string());
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    dirs
}

/// The `name` attribute of a `<param>`.
fn param_name(e: &BytesStart) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.as_ref() == "name")
            .then(|| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok())
            .flatten()
            .map(|v| v.into_owned())
    })
}

/// Remove the untracked directories a service run left at the top of the
/// checkout, when the checkout's `_service` names them as a source directory
/// ([`service_source_dirs`]). A directory is removed only when it is not a dot
/// entry, OBS does not track it, the expanded spec (`named`) does not name it,
/// its name is a plain file argument and it is a real directory (a symbolic
/// link is never followed). Returns the names removed.
pub fn remove_service_source_dirs(checkout_dir: &Path, named: &[String]) -> Vec<String> {
    let Ok(xml) = fs::read_to_string(checkout_dir.join("_service")) else {
        return Vec::new();
    };
    let tracked = tracked_files(checkout_dir);
    let mut removed = Vec::new();
    for name in service_source_dirs(&xml) {
        if name.starts_with('.')
            || tracked.contains(&name)
            || named.iter().any(|p| matches_source_name(p, &name))
            || !is_safe_file_arg(&name)
        {
            continue;
        }
        let path = checkout_dir.join(&name);
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            removed.push(name);
        }
    }
    removed
}
