use super::*;

#[test]
fn code_running_macros_are_recognised() {
    assert_eq!(code_macro_marker("Summary: %(id -u)"), Some("%("));
    assert_eq!(
        code_macro_marker("%global x %{lua: print(1)}"),
        Some("%{lua:")
    );
    assert_eq!(
        code_macro_marker("%description\nuse %{name} and %{python_module foo}"),
        None
    );
}

/// A spec whose `Summary` runs `touch` on expansion must not create the
/// marker on the host. With bubblewrap the expansion runs in a sandbox
/// where the marker's directory does not exist; without it the spec is
/// refused before rpmspec sees it.
#[test]
fn spec_macro_cannot_touch_a_marker_on_the_host() {
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("pwned");
    let work = tempfile::tempdir().unwrap();
    let spec = work.path().join("demo.spec");
    let text = format!(
        "Name: demo\nVersion: 1.0\nRelease: 0\nSummary: %(touch {}; echo x)\n\
             License: MIT\n%description\nx\n%files\n",
        marker.display()
    );
    fs::write(&spec, text).unwrap();

    let result = rpmspec_check(&spec, Emit::quiet());

    assert!(!marker.exists(), "spec macro ran on the host");
    if rpmspec_sandbox_works() {
        let out = result.expect("rpmspec should expand inside the sandbox");
        assert!(out.contains("Summary: x"), "{out}");
    }
}

#[test]
fn bare_lua_is_refused_but_luajit_is_not() {
    // `%luajit` is an ordinary macro name, not the Lua expansion form.
    assert_eq!(code_macro_marker("BuildRequires: %luajit-devel"), None);
    assert_eq!(code_macro_marker("%global v %luajit_version"), None);
    // A bare `%lua` at the end, before a space, or before `:` is refused.
    assert_eq!(code_macro_marker("Summary: %lua"), Some("%lua"));
    assert_eq!(code_macro_marker("Summary: %lua x"), Some("%lua"));
    assert_eq!(code_macro_marker("Summary: %lua:x"), Some("%lua"));
    assert_eq!(code_macro_marker("Summary: %lua{x}"), Some("%lua"));
    assert_eq!(code_macro_marker("Summary: %lua(x)"), Some("%lua"));
    // A later bare `%lua` after a `%luajit` is still found.
    assert_eq!(code_macro_marker("%luajit %lua x"), Some("%lua"));
    // The braced form stays refused.
    assert_eq!(code_macro_marker("%{lua:x}"), Some("%{lua:"));
    assert_eq!(
        code_macro_marker("%global x %{lua: print(1)}"),
        Some("%{lua:")
    );
    assert_eq!(unsandboxed_marker("Summary: %lua"), Some("%lua"));
}

#[test]
fn escaped_percent_is_text_and_a_real_macro_stays_refused() {
    // `%%` is a literal `%`: these are text, not code.
    assert_eq!(code_macro_marker("Summary: %%(id -u)"), None);
    assert_eq!(code_macro_marker("%%{lua:x}"), None);
    assert_eq!(code_macro_marker("Summary: %%lua"), None);
    assert_eq!(code_macro_marker("%%%%(id)"), None);
    // An odd `%` before the macro is a real one.
    assert_eq!(code_macro_marker("%%%(id -u)"), Some("%("));
    assert_eq!(code_macro_marker("%%%{lua:x}"), Some("%{lua:"));
    assert_eq!(code_macro_marker("%%%lua x"), Some("%lua"));
}

#[test]
fn file_includes_are_recognised_at_line_start() {
    assert_eq!(include_marker("%include /etc/passwd\n"), Some("%include"));
    assert_eq!(include_marker("  \t%include /etc/passwd"), Some("%include"));
    assert_eq!(
        include_marker("Name: x\n%{include /etc/passwd}\n"),
        Some("%{include")
    );
    assert_eq!(
        include_marker("Name: x\r\n%include /etc/passwd\r\n"),
        Some("%include")
    );
    assert_eq!(include_marker("Summary: see %include docs\n"), None);
    assert_eq!(include_marker("%description\nno includes here\n"), None);
    assert_eq!(include_marker("%include\r\n"), Some("%include"));
    assert_eq!(include_marker("  \t%include\t/x\n"), Some("%include"));
    assert_eq!(include_marker("%files\n%{includedir}/foo\n"), None);
    assert_eq!(include_marker("%files\n%{_includedir}/x\n"), None);
    assert_eq!(include_marker("%includes_foo\n"), None);
}

#[test]
fn unsandboxed_refusal_covers_includes_and_code_macros() {
    assert_eq!(
        unsandboxed_marker("Name: x\n%include /etc/hostname\n"),
        Some("%include")
    );
    assert_eq!(unsandboxed_marker("Summary: %(id -u)"), Some("%("));
    assert_eq!(
        unsandboxed_marker("Name: demo\nVersion: 1.0\n%description\nplain\n%files\n"),
        None
    );
}

#[test]
fn include_refusal_names_the_directive_and_the_spec() {
    let err = Error::UnsandboxedMacro {
        spec: "/co/demo.spec".into(),
        marker: "%include".into(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("/co/demo.spec") && msg.contains("`%include`"),
        "{msg}"
    );
}

#[test]
fn refusal_names_the_macro_and_the_spec() {
    let err = Error::UnsandboxedMacro {
        spec: "/co/demo.spec".into(),
        marker: "%(".into(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("/co/demo.spec") && msg.contains("`%(`"),
        "{msg}"
    );
}
