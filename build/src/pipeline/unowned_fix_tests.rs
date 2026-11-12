use super::unowned_fix::own_unowned_dirs;
use crate::log::unowned::unowned_dirs;

const LOG: &str = "[  99s] jay-compositor-1.15.0-1.1.x86_64.rpm: directories not owned by a package:\n\
    [  99s]  - /usr/share/wayland-sessions\n\
    [  99s]  - /usr/share/xdg-desktop-portal\n\
    [  99s]  - /usr/share/xdg-desktop-portal/portals\n\
    [  99s] ERROR: check-filelist failed\n";

fn spec(files: &str) -> String {
    format!(
        "Name:           jay-compositor\nVersion:        1.15.0\n\n\
         %description\nA compositor.\n\n\
         %package        devel\nSummary:        Dev\n\n\
         %description    devel\nDev.\n\n\
         %install\n%make_install\n\n%files\n{files}\n%changelog\n"
    )
}

fn fixed(spec: &str, log: &str) -> Option<String> {
    own_unowned_dirs(spec, &unowned_dirs(log)).map(|f| f.spec)
}

const JAY: &str = "%{_bindir}/jay\n\
    %{_datadir}/wayland-sessions/jay.desktop\n\
    %{_datadir}/xdg-desktop-portal/portals/jay.portal\n\
    %{_datadir}/xdg-desktop-portal/jay-portals.conf\n";

#[test]
fn the_jay_log_puts_each_dir_above_its_first_file_with_no_comment() {
    let after = fixed(&spec(JAY), LOG).unwrap();
    assert!(after.contains(
        "%files\n%{_bindir}/jay\n\
         %dir %{_datadir}/wayland-sessions\n\
         %{_datadir}/wayland-sessions/jay.desktop\n\
         %dir %{_datadir}/xdg-desktop-portal\n\
         %dir %{_datadir}/xdg-desktop-portal/portals\n\
         %{_datadir}/xdg-desktop-portal/portals/jay.portal\n\
         %{_datadir}/xdg-desktop-portal/jay-portals.conf\n\n%changelog\n"
    ));
    assert!(!after.contains("# "));
}

#[test]
fn a_second_run_changes_nothing() {
    let once = fixed(&spec(JAY), LOG).unwrap();
    assert_eq!(fixed(&once, LOG), None);
}

#[test]
fn an_existing_dir_line_is_not_repeated() {
    let files = "%dir %{_datadir}/wayland-sessions\n\
        %{_datadir}/wayland-sessions/jay.desktop\n";
    let log = "x.rpm: directories not owned by a package:\n - /usr/share/wayland-sessions\n";
    assert_eq!(fixed(&spec(files), log), None);
}

#[test]
fn the_parent_comes_before_the_child_when_the_log_lists_the_child_first() {
    let log = "x.rpm: directories not owned by a package:\n - /usr/share/a/b\n - /usr/share/a\n";
    let after = fixed(&spec("%{_datadir}/a/b/c\n"), log).unwrap();
    assert!(after.contains("%dir %{_datadir}/a\n%dir %{_datadir}/a/b\n%{_datadir}/a/b/c\n"));
}

#[test]
fn an_entry_of_a_subpackage_gets_the_line_in_its_own_section() {
    let spec = format!(
        "{}%files devel\n%{{_includedir}}/jay/jay.h\n\n",
        spec("%{_bindir}/jay\n")
    );
    let log = "x.rpm: directories not owned by a package:\n - /usr/include/jay\n";
    let after = fixed(&spec, log).unwrap();
    assert!(after.contains("%files devel\n%dir %{_includedir}/jay\n%{_includedir}/jay/jay.h\n"));
    assert!(after.contains("%files\n%{_bindir}/jay\n\n"));
}

#[test]
fn crlf_lines_keep_their_ending() {
    let crlf = spec(JAY).replace('\n', "\r\n");
    let after = fixed(&crlf, LOG).unwrap();
    assert!(after.contains(
        "\r\n%dir %{_datadir}/wayland-sessions\r\n%{_datadir}/wayland-sessions/jay.desktop\r\n"
    ));
    assert_eq!(after.matches('\n').count(), after.matches("\r\n").count());
}

#[test]
fn a_glob_entry_takes_the_line_above_it() {
    let log = "x.rpm: directories not owned by a package:\n - /usr/share/foo\n";
    let after = fixed(&spec("%{_bindir}/jay\n%{_datadir}/foo/*.conf\n"), log).unwrap();
    assert!(after.contains("%{_bindir}/jay\n%dir %{_datadir}/foo\n%{_datadir}/foo/*.conf\n"));
}

#[test]
fn a_dir_no_entry_lies_in_goes_to_the_end_of_the_nearest_section() {
    let spec = format!(
        "{}%files devel\n%{{_includedir}}/jay/jay.h\n\n",
        spec("%{_bindir}/jay\n")
    );
    let log = "x.rpm: directories not owned by a package:\n - /usr/include/jay/extra\n";
    let after = fixed(&spec, log).unwrap();
    assert!(after.contains("%{_includedir}/jay/jay.h\n%dir %{_includedir}/jay/extra\n\n"));
    assert_eq!(fixed(&after, log), None);
}

#[test]
fn a_path_that_is_not_in_the_log_shape_is_left_alone() {
    let log = "x.rpm: directories not owned by a package:\n - relative/dir\n";
    assert_eq!(fixed(&spec(JAY), log), None);
    assert_eq!(fixed(&spec(JAY), "nothing here\n"), None);
}

#[test]
fn a_standard_dir_is_never_owned() {
    let log =
        "x.rpm: directories not owned by a package:\n - /usr/bin\n - /usr/share/applications\n";
    let files = "%{_bindir}/jay\n%{_datadir}/applications/jay.desktop\n";
    assert_eq!(fixed(&spec(files), log), None);
    let files = "%{_bindir}/jay\n";
    assert_eq!(fixed(&spec(files), log), None);
}

#[test]
fn the_fallback_line_stays_out_of_the_next_sections_conditional() {
    let text = "Name: x\nVersion: 1\n\n%description\nx\n\n%files\n%{_bindir}/x\n\n\
                %if 0%{?y}\n%package -n z\nSummary: z\n%description -n z\nz\n%endif\n";
    let log = "x.rpm: directories not owned by a package:\n - /usr/bin/zzz\n";
    let after = fixed(text, log).unwrap();
    assert!(after.contains("%files\n%{_bindir}/x\n%dir %{_bindir}/zzz\n\n%if"));
}

#[test]
fn every_section_with_an_entry_in_the_dir_gets_a_line() {
    let both = format!(
        "{}%files devel\n%{{_includedir}}/jay/jay.h\n\n",
        spec("%dir %{_includedir}/jay\n%{_includedir}/jay/main.h\n")
    );
    let log = "x.rpm: directories not owned by a package:\n - /usr/include/jay\n";
    let after = fixed(&both, log).unwrap();
    assert!(after.contains("%files devel\n%dir %{_includedir}/jay\n%{_includedir}/jay/jay.h\n"));
    assert_eq!(after.matches("%dir %{_includedir}/jay\n").count(), 2);
    assert_eq!(fixed(&after, log), None);
    let neither = format!(
        "{}%files devel\n%{{_includedir}}/jay/jay.h\n\n",
        spec("%{_includedir}/jay/main.h\n")
    );
    let after = fixed(&neither, log).unwrap();
    assert_eq!(after.matches("%dir %{_includedir}/jay\n").count(), 2);
}
