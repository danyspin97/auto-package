use super::*;
use crate::osc::sandbox::*;

#[test]
fn a_relative_spec_is_bound_from_the_current_directory() {
    let cwd = Path::new("/work");
    assert_eq!(
        sandbox_spec_dir(Path::new("pkg/foo.spec"), cwd),
        PathBuf::from("/work/pkg")
    );
    assert_eq!(
        sandbox_spec_dir(Path::new("foo.spec"), cwd),
        PathBuf::from("/work")
    );
}

#[test]
fn an_absolute_spec_is_bound_from_its_own_parent() {
    assert_eq!(
        sandbox_spec_dir(Path::new("/a/b/foo.spec"), Path::new("/work")),
        PathBuf::from("/a/b")
    );
}
