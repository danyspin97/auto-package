//! Whether the branch project's build path lists a binary name, read from the
//! repositories themselves (D-20261102-34). The search that `judge` works from
//! finds only some of the projects that publish a name: autoconf is listed by
//! openSUSE:Factory, yet the search returned only old projects for it. So a
//! "BuildRequires X is not published" warning needs this listing to lack the
//! name, and a name a listing fails to read is not evidence either.

use super::judge::reachable_pairs;
use super::*;

impl DepResolver {
    /// Whether a repository on the build path of the branch project, for its
    /// architecture, lists a binary called `name`. `Err` when a listing fails
    /// and no other one lists it: the answer is unknown, not "missing".
    pub fn in_build_path(&self, name: &str) -> Result<bool, String> {
        let (_, info) = self.branch_info()?;
        let mut failed = None;
        for repo in info
            .repositories
            .iter()
            .filter(|r| r.arches.contains(&self.arch))
        {
            let pairs = reachable_pairs(&self.branch_project, &info, &repo.name, &mut |p| {
                self.path_project_info(p)
            });
            for (project, path_repository) in pairs {
                match self.repository_names(&project, &path_repository) {
                    Ok(names) if names.iter().any(|n| n == name) => return Ok(true),
                    Ok(_) => {}
                    Err(e) => failed = failed.or(Some(e)),
                }
            }
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(false),
        }
    }

    /// Whether this is the first warning about `name` in the run: a name is
    /// reported once, however many packages need it.
    pub fn first_report(&self, name: &str) -> bool {
        self.reported
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string())
    }
}
