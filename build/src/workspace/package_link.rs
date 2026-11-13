//! The `package` link of a workspace: the osc checkout directory that holds the
//! spec. The turn lists and reads the package's other files (patches, `_service`,
//! `.changes`) through it with a relative path, which the sandbox allows, where
//! `ls` of the absolute checkout path is refused.

use super::Workspace;
use std::path::Path;

impl Workspace {
    /// Create or refresh the `package` symlink to the directory that holds
    /// `spec_path` (the checkout's spec, absolute). Swapped in one rename, as
    /// [`Workspace::link_spec`] is.
    #[cfg(unix)]
    pub fn link_package(&self, spec_path: &Path) -> std::io::Result<()> {
        let Some(dir) = spec_path.parent() else {
            return Err(std::io::Error::other("the spec path has no directory"));
        };
        ap_core::fsx::replace_symlink(dir, &self.dir.join("package"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_link_points_at_the_checkout_directory_and_is_refreshed() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            dir: tmp.path().join("state"),
        };
        std::fs::create_dir_all(&ws.dir).unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        for co in [&first, &second] {
            std::fs::create_dir_all(co).unwrap();
            std::fs::write(co.join("pkg.spec"), "spec").unwrap();
            std::fs::write(co.join("pkg.changes"), "changes").unwrap();
        }
        let link = ws.dir.join("package");

        ws.link_package(&first.join("pkg.spec")).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(link.join("pkg.changes")).unwrap(),
            "changes"
        );

        ws.link_package(&second.join("pkg.spec")).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), second);
    }
}
