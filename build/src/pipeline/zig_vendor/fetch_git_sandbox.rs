//! The real [`GitRun`]: each `git` command runs in the service sandbox, the way
//! the `cargo_vendor` helper runs. Inside, the environment is the sandbox's: an
//! empty HOME, no system or global config, no credential helper, no prompt, and
//! `GIT_ALLOW_PROTOCOL=https`. The network is the CONNECT proxy, which allows
//! only the dependency's host on port 443 (a proxy started with
//! [`Proxy::start_only`], whatever the allowlist says). Each command gets a
//! proxy of its own, so a refusal is reported with the command that made it.
//! No file the command writes may grow past `max_file` bytes: `prlimit` sets
//! that limit for the sandbox and everything it runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use super::fetch_git::GitRun;
use crate::allowed_hosts::Asker;
use crate::proc::{self, Emit};
use crate::service_sandbox::{Profile, Proxy, bridge_addresses, bwrap_args, sandbox_ready};

/// The sandbox's own scratch directory, inside the directory it runs in. Not
/// part of any exported tree.
pub(super) const SCRATCH: &str = ".ap-sandbox";

/// Sets the resource limits of the program it runs. Part of util-linux.
const PRLIMIT: &str = "/usr/bin/prlimit";

/// The `prlimit` arguments that cap a sandboxed command: `max_file` bytes per
/// file, 4096 open files, 4 GiB of address space and ten minutes of CPU. There
/// is no process limit: RLIMIT_NPROC counts every task of the user, threads
/// included (1712 on the machine this was written on), so any cap below that
/// stops every fork of the user, not only the sandbox's.
pub(super) fn prlimit_args(max_file: u64) -> Vec<String> {
    [
        format!("--fsize={max_file}"),
        "--nofile=4096".to_string(),
        format!("--as={}", 4u64 << 30),
        "--cpu=600".to_string(),
        "--".to_string(),
    ]
    .into()
}

/// Runs git in the sandbox.
pub(super) struct SandboxGit {
    /// The most bytes any one file a command writes may grow to. A fetch keeps
    /// its objects in one pack file (see `fetch.unpackLimit`), so this bounds
    /// the objects as they arrive, not only after the fetch.
    pub(super) max_file: u64,
}

impl GitRun for SandboxGit {
    fn run(
        &self,
        package: &str,
        host: &str,
        work: &Path,
        args: &[String],
    ) -> Result<String, String> {
        if !Path::new(PRLIMIT).is_file() {
            return Err(format!("{PRLIMIT} is not installed: git cannot be limited"));
        }
        let bwrap = sandbox_ready()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let checkout = fs::canonicalize(work).map_err(|e| format!("{}: {e}", work.display()))?;
        let scratch = checkout.join(SCRATCH);
        for sub in ["tmp", "cache", "cargo", "npm", "go"] {
            fs::create_dir_all(scratch.join(sub)).map_err(|e| e.to_string())?;
        }
        let asker = Asker::service(package, "zig_vendor");
        let hosts = crate::allowed_hosts::shared();
        let proxy = Proxy::start_only(Arc::clone(&hosts), asker.clone(), host)
            .map_err(|e| e.to_string())?;
        let map = bridge_addresses(&[host.to_string()], &hosts, &asker)?;
        let hosts_file = proxy
            .write_hosts("hosts", &map)
            .map_err(|e| e.to_string())?;
        let extra: Vec<(String, String)> = [
            ("GIT_ALLOW_PROTOCOL", "https"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_LFS_SKIP_SMUDGE", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into();
        let profile = Profile {
            checkout: &checkout,
            scratch: &scratch,
            proxy_dir: proxy.dir(),
            extra_env: &extra,
            ro_binds: &[],
            hosts_file: &hosts_file,
            tunnels: &map,
        };
        let mut argv = prlimit_args(self.max_file);
        argv.push(bwrap);
        argv.extend(bwrap_args(&profile, args)?);
        let out = proc::capture_with_timeout_env(
            PRLIMIT,
            &argv,
            &checkout,
            Emit::quiet(),
            proc::DEFAULT_CAPTURE_TIMEOUT,
            Some(&BTreeMap::new()),
        )
        .map_err(|e| format!("running git in the sandbox: {e}"))?;
        if out.success() {
            return Ok(out.stdout);
        }
        let mut err = out.stderr.trim_end().to_string();
        if err.is_empty() {
            err = format!(
                "the command failed with no output (a file may have passed {} bytes)",
                self.max_file
            );
        }
        for r in proxy.refusals() {
            err.push_str(&format!("\nsandbox proxy refused: {r}"));
        }
        Err(err)
    }
}
