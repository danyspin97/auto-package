use super::*;
use std::io;

const PREFIX: &str = "/home/u/.local";

fn unsafe_prefix() -> Error {
    Error::Sandbox(SandboxError::UnsafePrefix {
        program: "pip".into(),
        prefix: PREFIX.into(),
        why: "it is inside a configuration or cache directory and is not a virtualenv or Python installation",
    })
}

fn missing() -> Error {
    Error::Launch(io::Error::new(io::ErrorKind::NotFound, "not on PATH"))
}

fn alternates() -> Vec<String> {
    vec!["python3.12".into(), "python3.11".into()]
}

#[test]
fn unsafe_default_prefix_moves_on_to_the_next_interpreter() {
    // ~/.local/bin/pip is refused, but the alternate that works is used.
    let mut tried = Vec::new();
    let (alternate, out) = select_candidate(Err(unsafe_prefix()), &alternates(), |py| {
        tried.push(py.to_string());
        if py == "python3.12" {
            Ok(b"report".to_vec())
        } else {
            Err(missing())
        }
    })
    .unwrap();
    assert_eq!(alternate.as_deref(), Some("python3.12"));
    assert_eq!(out, b"report");
    assert_eq!(tried, ["python3.12"]);
}

#[test]
fn unsafe_prefix_on_every_candidate_names_the_prefix() {
    let err = select_candidate(
        Err(unsafe_prefix()),
        &alternates(),
        |_| Err(unsafe_prefix()),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::Sandbox(SandboxError::UnsafePrefix { .. })),
        "{err}"
    );
    assert!(err.to_string().contains(PREFIX), "{err}");
}

#[test]
fn incompatible_python_still_moves_on() {
    let incompatible = || Error::IncompatiblePython {
        name: "open-webui".into(),
        example: "0.8.12".into(),
    };
    let (alternate, _) = select_candidate(Err(incompatible()), &alternates(), |py| {
        if py == "python3.11" {
            Ok(Vec::new())
        } else {
            Err(incompatible())
        }
    })
    .unwrap();
    assert_eq!(alternate.as_deref(), Some("python3.11"));
}

#[test]
fn other_default_failures_are_final() {
    let mut tried = false;
    let err = select_candidate(Err(Error::NotFound("nope".into())), &alternates(), |_| {
        tried = true;
        Ok(Vec::new())
    })
    .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    assert!(
        !tried,
        "no alternate is tried after a final default failure"
    );
}
