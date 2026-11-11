//! Why a carry-forward of the recipe failed: the reason, and the remedy when
//! the machine lacks a tool the carry needs (a service's program, a toolchain).
//! Such a failure ends the package as an environment failure; any other goes
//! on to the usual fallback.

use super::*;

#[derive(Debug)]
pub(super) struct CarryError {
    pub(super) why: String,
    pub(super) remedy: Option<ap_core::remedy::Remedy>,
}

impl From<String> for CarryError {
    fn from(why: String) -> Self {
        CarryError { why, remedy: None }
    }
}

impl From<&str> for CarryError {
    fn from(why: &str) -> Self {
        CarryError::from(why.to_string())
    }
}

impl CarryError {
    /// A `_service` run the carry needed that failed: its remedy, when the
    /// machine lacks something the service runs.
    pub(super) fn service(e: &osc::Error) -> Self {
        CarryError {
            why: format!("running the _service: {e}"),
            remedy: e.remedy(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_missing_a_tool_keeps_its_remedy_and_its_text() {
        let err = CarryError::service(&osc::Error::ToolMissing {
            tool: "tar_scm".into(),
            path: None,
        });
        assert_eq!(
            err.why,
            "running the _service: tar_scm is not installed (could not launch it)"
        );
        assert!(err.remedy.is_some());
    }

    #[test]
    fn a_service_that_refused_keeps_no_remedy() {
        let err = CarryError::service(&osc::Error::OscFailed {
            cmd: "service ra".into(),
            code: 1,
            stderr: "boom".into(),
        });
        assert!(err.why.starts_with("running the _service: "));
        assert_eq!(err.remedy, None);
    }
}
