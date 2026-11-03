//! What openSUSE:Factory holds for a package name, and which devel project its
//! meta names. A read only: the answer tells a package of the source project
//! whether Factory has it from this project or from another.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;

/// What openSUSE:Factory holds for a package name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryPackage {
    /// Factory has no package of that name: OBS answered 404 for its meta.
    Absent,
    /// Factory has the package; `devel` is the devel project its meta names,
    /// `None` when the meta names none.
    Present { devel: Option<String> },
}

/// The read of openSUSE:Factory's `pkg` meta. A 404 is the answer
/// [`FactoryPackage::Absent`]; any other failure is an error, which is no answer.
pub fn factory_package(pkg: &str, emit: Emit<'_>) -> Result<FactoryPackage, Error> {
    check_obs_name("package", pkg)?;
    let path = format!("/source/openSUSE:Factory/{}/_meta", enc(pkg));
    let out = api_call(None, &path, None, emit)?;
    if !out.success() {
        if is_not_found(&out.stderr) {
            return Ok(FactoryPackage::Absent);
        }
        return Err(Error::OscFailed {
            cmd: format!("api {path}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(FactoryPackage::Present {
        devel: parse_devel_project(&out.stdout),
    })
}
