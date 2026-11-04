//! Superseding a request by a new one: what `osc sr --supersede <old>` does
//! after it created the new request. This is the only call that changes the
//! state of a request; the caller has created the new request first.

use super::*;
use crate::osc::error::*;

/// The path of the state change of request `old`, superseded by `new`. Both
/// ids are digits only.
pub fn supersede_path(old: &str, new: &str) -> Result<String, Error> {
    for (what, id) in [("old request", old), ("new request", new)] {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::NotActionable(format!(
                "refusing to use {what} {id:?}: not a request id"
            )));
        }
    }
    Ok(format!(
        "/request/{old}?cmd=changestate&newstate=superseded&superseded_by={new}"
    ))
}

/// Mark request `old` superseded by `new`, with `comment` as the reason (the
/// body of the call, as `osc` sends its message). The creator of a request may
/// supersede it; whether OBS also allows it for a declined one is not checked
/// here: OBS answers.
pub fn supersede_request(old: &str, new: &str, comment: &str, emit: Emit<'_>) -> Result<(), Error> {
    let path = supersede_path(old, new)?;
    let out = api_call(Some("POST"), &path, Some(comment), emit)?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::OscFailed {
            cmd: format!("api -X POST {path}"),
            code: out.code,
            stderr: out.stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_changes_the_state_and_names_the_new_request() {
        assert_eq!(
            supersede_path("1394662", "1394999").unwrap(),
            "/request/1394662?cmd=changestate&newstate=superseded&superseded_by=1394999"
        );
    }

    #[test]
    fn an_id_that_is_not_digits_is_refused() {
        for (old, new) in [("", "1"), ("1", ""), ("1/../2", "3"), ("1", "3&force=1")] {
            assert!(supersede_path(old, new).is_err(), "{old:?} {new:?}");
        }
    }
}
