//! Packages of the source project that openSUSE:Factory has from another devel
//! project than the source. Such a package is left to the maintainer, listed
//! and not branched, unless `--deprecated` asks for it.
//!
//! Only a positive answer leaves a package out: Factory has the package and its
//! meta names a different devel project. Not seen, and so never skipped: a
//! package Factory does not have (new here, or dropped by Factory, which the
//! read cannot tell apart), a Factory package whose meta names no devel project,
//! a failed read, and a package Factory keeps under another name.

use super::*;

const FACTORY: &str = "openSUSE:Factory";

/// What openSUSE:Factory says about a package of the source project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FactoryDevel {
    /// The source is openSUSE:Factory itself, or Factory's package names the
    /// source as its devel project.
    Source,
    /// Factory's package names this other devel project.
    Other(String),
}

/// The positive answer about `source` from Factory's read of the package
/// (`osc::factory_package`). `None` when there is no positive answer.
pub(super) fn factory_devel(
    source: &str,
    answer: Result<osc::FactoryPackage, String>,
) -> Option<FactoryDevel> {
    if source == FACTORY {
        return Some(FactoryDevel::Source);
    }
    match answer {
        Ok(osc::FactoryPackage::Present { devel: Some(devel) }) if devel == source => {
            Some(FactoryDevel::Source)
        }
        Ok(osc::FactoryPackage::Present { devel: Some(devel) }) => Some(FactoryDevel::Other(devel)),
        Ok(osc::FactoryPackage::Present { devel: None })
        | Ok(osc::FactoryPackage::Absent)
        | Err(_) => None,
    }
}

/// Factory's read of `pkg`, made once per run: an answer is kept for the package,
/// a failed read is not, so it is asked again.
fn factory_package_once(pkg: &str, emit: Emit<'_>) -> Result<osc::FactoryPackage, String> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static KNOWN: LazyLock<Mutex<HashMap<String, osc::FactoryPackage>>> =
        LazyLock::new(Default::default);
    let lock = || KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(answer) = lock().get(pkg).cloned() {
        return Ok(answer);
    }
    let answer = osc::factory_package(pkg, emit).map_err(|e| e.to_string())?;
    lock().insert(pkg.to_string(), answer.clone());
    Ok(answer)
}

/// The devel project a package is left out for, or `None` when it is not left
/// out: Factory's devel project is not another one, or `--deprecated` includes it.
pub(super) fn skipped_devel(state: Option<FactoryDevel>, deprecated: bool) -> Option<String> {
    match state {
        Some(FactoryDevel::Other(devel)) if !deprecated => Some(devel),
        _ => None,
    }
}

/// Whether the check is made for an item at all: a package of the source
/// itself, in a normal update. A dependency from another project
/// (`--update-deps`), a source that is openSUSE:Factory, `--in-place`, lint and
/// the maintenance runs are not checked.
pub(super) fn applies(item_origin: &str, a: &UpdateProjectArgs) -> bool {
    item_origin == a.source
        && a.source != FACTORY
        && !a.in_place
        && !a.improve
        && !a.maintenance_source
}

/// The argument checks of `--deprecated`: it has no meaning on a maintenance
/// branch yet, and only `update` takes it.
pub(super) fn check_args(a: &UpdateProjectArgs) -> Result<(), String> {
    if !a.deprecated {
        return Ok(());
    }
    if a.maintenance.is_some() || a.maintenance_list {
        return Err("--deprecated is not supported with --maintenance yet".to_string());
    }
    if a.improve {
        return Err("--deprecated is only for update".to_string());
    }
    Ok(())
}

/// Whether `item` is left out as Factory's package of it comes from another devel
/// project. When it is, it is warned about and listed under the maintainer's
/// heading, in the dry run and in the summary alike; nothing is branched for it.
pub(super) fn settled_as_deprecated(
    item: &Item,
    a: &UpdateProjectArgs,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> bool {
    if !applies(&item.origin, a) {
        return false;
    }
    // The read is made only when its answer could leave the package out.
    let state = if a.deprecated {
        None
    } else {
        let emit = Emit::run(ctx.reporter.as_ref());
        factory_devel(&a.source, factory_package_once(&item.pkg, emit))
    };
    let Some(devel) = skipped_devel(state, a.deprecated) else {
        return false;
    };
    let why = update::SkipReason::Deprecated {
        devel,
        source: a.source.clone(),
    }
    .to_string();
    ctx.warn(format!("{}: {why}", item.pkg));
    report.maintainer.push(format!("{}: {why}", item.pkg));
    true
}

#[cfg(test)]
#[path = "deprecated_tests.rs"]
mod tests;
