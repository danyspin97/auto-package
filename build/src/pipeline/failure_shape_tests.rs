use super::{failure_shape, same_shape_note};

fn pmda_failure(name: &str, tmp: &str, line: u32) -> crate::log::ExtractedLog {
    crate::log::ExtractedLog {
        failed_section: Some("%install".into()),
        excerpt: format!(
            "+ cd /home/abuild/rpmbuild/BUILD/pcp-7.2.2-build/BUILDROOT/var/lib/pcp/pmdas/{name}\n\
                 /var/tmp/rpm-tmp.{tmp}: line {line}: cd: /home/abuild/rpmbuild/BUILD/pcp-7.2.2-build/BUILDROOT/var/lib/pcp/pmdas/{name}: No such file or directory"
        ),
        ..Default::default()
    }
}

#[test]
fn the_same_error_for_another_directory_has_the_same_shape() {
    let mic = failure_shape(&pmda_failure("mic", "l1ijiw", 389)).unwrap();
    let json = failure_shape(&pmda_failure("json", "ys7EhM", 497)).unwrap();
    assert_eq!(mic, json);
}

#[test]
fn a_different_error_has_another_shape() {
    let other = crate::log::ExtractedLog {
        failed_section: Some("%build".into()),
        excerpt: "foo.c:12:3: error: unknown type name 'bar'".into(),
        ..Default::default()
    };
    assert_ne!(
        failure_shape(&other),
        failure_shape(&pmda_failure("mic", "a", 1))
    );
    assert_eq!(failure_shape(&Default::default()), None);
}

#[test]
fn the_note_starts_with_the_second_failure_of_a_shape() {
    assert!(same_shape_note(0).is_none());
    let note = same_shape_note(1).unwrap();
    assert!(note.contains("number 2 in a row"), "{note}");
    assert!(note.contains("every other instance"), "{note}");
}
