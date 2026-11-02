//! Whether a branch project's build paths can supply a package's
//! `BuildRequires`, and which extra path to add when they cannot.
//!
//! A package branched out of, say, `Base:System` may need packages that live
//! in another project (`devel:languages:c_c++`). The branch project is built
//! against its repository paths, so such a package is only found if one of the
//! paths — directly or through the paths of the paths — publishes it.
//!
//! The check asks OBS for the published binaries of each name once (a search
//! across every project, cached for the run) and compares the projects that
//! publish it with the projects the branch project's paths reach. Reachability
//! is decided per project, not per repository: a distro project publishes
//! under repository names (`standard`, `snapshot`) that its dependents name
//! differently, and what matters is that the project is on the path chain.
//!
//! Only a name that is a binary package name can be looked up this way. A
//! capability (`pkgconfig(glib-2.0)`) is reported as unchecked; the build
//! catches those, and `cover_dependency` handles them afterwards.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ap_core::event::Event;
use ap_core::sink::Reporter;

use crate::osc::{self, PathAddition, ProjectInfo, PublishedBinary};
use crate::proc::Emit;
use crate::spec::BuildRequire;

mod build_path;
#[cfg(test)]
mod build_path_tests;
mod capability;
#[cfg(test)]
mod capability_tests;
mod flags;
mod judge;
mod obs;
mod requirements;
#[cfg(test)]
mod requirements_limit_tests;
#[cfg(test)]
mod requirements_tests;
mod resolver;
#[cfg(test)]
mod tests;

pub use capability::*;
pub use flags::*;
pub use judge::*;
pub use obs::*;
pub use resolver::*;
