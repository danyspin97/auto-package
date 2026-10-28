use super::*;

/// Remove what an earlier run generated, as osc does before running.
pub(super) fn remove_generated(checkout: &Path) {
    let Ok(rd) = fs::read_dir(checkout) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("_service:") || name.starts_with("_service_") {
            let path = entry.path();
            let _ = if path.is_dir() && !path.is_symlink() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

pub(super) fn top_level_symlinks(dir: &Path) -> Vec<std::ffi::OsString> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                .map(|e| e.file_name())
                .collect()
        })
        .unwrap_or_default()
}

/// Move a service's outputs into the checkout, as osc does. A symbolic link
/// (which could point the pipeline at a host file) and `.osc` are refused.
pub(super) fn move_outputs(service: &str, outdir: &Path, checkout: &Path) -> Result<(), Error> {
    let rejected = |reason: String| Error::ServiceRejected { reason };
    for entry in fs::read_dir(outdir).map_err(Error::Io)?.flatten() {
        let name = entry.file_name();
        if entry.file_type().map_err(Error::Io)?.is_symlink() {
            return Err(rejected(format!(
                "service {service} produced a symbolic link `{}`",
                name.to_string_lossy()
            )));
        }
        if name == ".osc" {
            return Err(rejected(format!("service {service} produced `.osc`")));
        }
        let target = checkout.join(&name);
        if target.is_dir() && !target.is_symlink() {
            fs::remove_dir_all(&target).map_err(Error::Io)?;
        }
        fs::rename(entry.path(), &target).map_err(Error::Io)?;
    }
    Ok(())
}

/// Run `services` (from the checkout's validated `_service`) in the sandbox,
/// in order, each the way `osc service ra` would. The result is the failing
/// service's output, with the proxy's refusals appended, or a success.
pub(crate) fn run_services(
    checkout_dir: &Path,
    services: &[ServiceCall],
    emit: Emit<'_>,
) -> Result<Captured, Error> {
    let ok = Captured {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    if services.is_empty() {
        return Ok(ok);
    }
    let bwrap = sandbox_ready()?;
    for s in services {
        if !Path::new(SERVICE_DIR).join(&s.name).is_file() {
            return Err(Error::ServiceSandbox(format!(
                "obs-service-{} is not installed ({SERVICE_DIR}/{} is missing)",
                s.name, s.name
            )));
        }
    }
    let checkout = fs::canonicalize(checkout_dir).map_err(Error::Io)?;
    remove_generated(&checkout);
    let links_before = top_level_symlinks(&checkout);
    let scratch = tempfile::Builder::new()
        .prefix(".ap-service-")
        .tempdir_in(&checkout)
        .map_err(Error::Io)?;
    for sub in ["tmp", "cache", "cargo", "npm", "go"] {
        fs::create_dir_all(scratch.path().join(sub)).map_err(Error::Io)?;
    }
    let proxy = Proxy::start(extra_source_hosts()).map_err(Error::Io)?;
    let identity = changes_identity(&checkout);
    let profile = Profile {
        checkout: &checkout,
        scratch: scratch.path(),
        proxy_dir: proxy.dir(),
        extra_env: &identity,
    };
    let cmd = format!("service ra in {}", checkout_dir.display());
    let bwrap = bwrap.to_string_lossy();
    for s in services {
        let outdir = tempfile::Builder::new()
            .prefix(".")
            .suffix(&format!(".{}.service", s.name))
            .tempdir_in(&checkout)
            .map_err(Error::Io)?;
        let args =
            bwrap_args(&profile, &service_argv(s, outdir.path())).map_err(Error::ServiceSandbox)?;
        let out = proc::capture_with_timeout_env(
            &bwrap,
            &args,
            &checkout,
            emit,
            proc::DEFAULT_CAPTURE_TIMEOUT,
            Some(&BTreeMap::new()),
        )
        .map_err(|source| Error::Command {
            cmd: cmd.clone(),
            source,
        })?;
        if !out.success() {
            let mut stderr = format!("service {} failed:\n{}", s.name, out.stderr.trim_end());
            for r in proxy.refusals() {
                stderr.push_str(&format!("\nsandbox proxy refused: {r}"));
            }
            return Ok(Captured {
                code: out.code,
                stdout: out.stdout,
                stderr,
            });
        }
        move_outputs(&s.name, outdir.path(), &checkout)?;
    }
    for name in top_level_symlinks(&checkout) {
        if !links_before.contains(&name) {
            let _ = fs::remove_file(checkout.join(&name));
            return Err(Error::ServiceRejected {
                reason: format!(
                    "a service created the symbolic link `{}` in the checkout",
                    name.to_string_lossy()
                ),
            });
        }
    }
    Ok(ok)
}
