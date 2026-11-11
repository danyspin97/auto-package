//! Which Rust toolchain a cargo service runs with, chosen from files on the
//! host (no rustup process, no network), so a rustup without a default
//! toolchain does not stop `cargo_vendor`.

use super::*;
use ap_core::event::Event;

/// What the choice is made from.
#[derive(Debug, Default)]
pub(crate) struct Inputs<'a> {
    /// The text of the source's `rust-toolchain` or `rust-toolchain.toml`.
    pub pin_file: Option<&'a str>,
    /// The entries of `<rustup home>/toolchains`.
    pub installed: &'a [String],
    /// `default_toolchain` of `<rustup home>/settings.toml`.
    pub host_default: Option<&'a str>,
    /// Whether `cargo` is the rustup proxy.
    pub cargo_is_rustup: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Run with the installed toolchain `name`. `warnings` say what was
    /// ignored or replaced on the way.
    Selected {
        name: String,
        why: String,
        warnings: Vec<String>,
    },
    /// `cargo` is a real binary: rustup is not involved.
    PlainCargo,
    /// rustup is there and no toolchain is installed.
    NoToolchain,
}

/// The channel a `rust-toolchain` file pins. `Err` when the file is
/// malformed; `Ok(None)` when it pins nothing (no channel, or a path).
pub(crate) fn pinned_channel(text: &str) -> Result<Option<String>, String> {
    let valid = |c: &str| {
        !c.is_empty()
            && c.len() <= 100
            && c.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let channel = if trimmed.contains('=') || trimmed.contains('[') {
        let table: toml::Table = toml::from_str(text).map_err(|e| format!("not TOML: {e}"))?;
        let Some(tc) = table.get("toolchain") else {
            return Err("no [toolchain] table".to_string());
        };
        match tc.get("channel") {
            Some(toml::Value::String(c)) => c.trim().to_string(),
            Some(_) => return Err("channel is not a string".to_string()),
            None => return Ok(None),
        }
    } else {
        let mut lines = trimmed.lines().map(str::trim).filter(|l| !l.is_empty());
        let first = lines.next().unwrap_or_default();
        if lines.next().is_some() {
            return Err("more than one line".to_string());
        }
        first.to_string()
    };
    if valid(&channel) {
        Ok(Some(channel))
    } else {
        Err(format!(
            "`{}` is not a toolchain name",
            channel.escape_debug()
        ))
    }
}

/// Whether `rest` looks like a target triple, `<arch>-<vendor>-<os>[-<env>]`
/// of letters, digits and underscores, the arch starting with a letter.
fn is_triple(rest: &str) -> bool {
    let parts: Vec<&str> = rest.split('-').collect();
    (3..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        && rest.starts_with(|c: char| c.is_ascii_alphabetic())
}

/// The triple of this machine as rustup names it on Linux, as far as the
/// architecture tells: `x86_64-unknown-linux-gnu`.
fn host_triple() -> String {
    format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
}

/// The installed toolchain that `want` names: `stable` for
/// `stable-x86_64-unknown-linux-gnu`. After the channel comes a target
/// triple, not a date or another word: `nightly` is not
/// `nightly-2024-01-01-...` and `stable` is not `stable-foo`. Of several
/// triples the host's wins, then the first by name. An empty `want` names
/// nothing.
pub(crate) fn installed_match<'a>(want: &str, installed: &'a [String]) -> Option<&'a str> {
    if want.is_empty() {
        return None;
    }
    let host_suffix = format!("-{}", host_triple());
    let mut hits: Vec<&str> = installed
        .iter()
        .map(String::as_str)
        .filter(|name| {
            *name == want
                || name
                    .strip_prefix(want)
                    .and_then(|r| r.strip_prefix('-'))
                    .is_some_and(is_triple)
        })
        .collect();
    hits.sort_unstable_by_key(|n| (!n.ends_with(&host_suffix), *n));
    hits.first().copied()
}

/// The toolchain to run cargo with.
pub(crate) fn select_toolchain(inputs: &Inputs<'_>) -> Choice {
    if !inputs.cargo_is_rustup {
        return Choice::PlainCargo;
    }
    let mut warnings = Vec::new();
    let mut pin = None;
    if let Some(text) = inputs.pin_file {
        match pinned_channel(text) {
            Ok(p) => pin = p,
            Err(e) => warnings.push(format!("ignoring the source's rust-toolchain file: {e}")),
        }
    }
    let found = |name: &str, why: String, warnings| Choice::Selected {
        name: name.to_string(),
        why,
        warnings,
    };
    if let Some(pin) = &pin {
        if let Some(name) = installed_match(pin, inputs.installed) {
            return found(name, format!("pinned by the source: {pin}"), warnings);
        }
        warnings.push(format!(
            "the source pins toolchain {pin}, which is not installed; falling back"
        ));
    }
    let fallback = if pin.is_some() { "fallback: " } else { "" };
    if let Some(name) = inputs
        .host_default
        .and_then(|d| installed_match(d, inputs.installed))
    {
        return found(name, format!("{fallback}the host default"), warnings);
    }
    if let Some(name) = installed_match("stable", inputs.installed) {
        return found(name, format!("{fallback}stable"), warnings);
    }
    Choice::NoToolchain
}

/// The variables a choice adds to the sandbox: nothing unless a toolchain is
/// selected. `RUSTUP_AUTO_INSTALL=0` keeps rustup from downloading one.
pub(crate) fn choice_env(choice: &Choice) -> Vec<(String, String)> {
    match choice {
        Choice::Selected { name, .. } => vec![
            ("RUSTUP_TOOLCHAIN".to_string(), name.clone()),
            ("RUSTUP_AUTO_INSTALL".to_string(), "0".to_string()),
        ],
        Choice::PlainCargo | Choice::NoToolchain => Vec::new(),
    }
}

const CARGO_PATHS: [&str; 2] = ["/usr/bin/cargo", "/bin/cargo"];
const PIN_FILES: [&str; 2] = ["rust-toolchain", "rust-toolchain.toml"];
const MAX_FILE: u64 = 64 * 1024;

/// Whether the `cargo` the sandbox's PATH finds resolves to rustup.
pub(crate) fn cargo_is_rustup(paths: &[&str]) -> bool {
    paths
        .iter()
        .map(Path::new)
        .find(|p| fs::symlink_metadata(p).is_ok())
        .and_then(|p| fs::canonicalize(p).ok())
        .is_some_and(|real| real.file_name().is_some_and(|n| n == "rustup"))
}

fn read_small(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The directory a cargo service reads its sources from, when the checkout
/// holds it (`src` or `srcdir`): a directory inside the checkout.
pub(crate) fn source_dir(params: &[(String, String)], checkout: &Path) -> Option<PathBuf> {
    let value = params
        .iter()
        .find(|(k, _)| k == "src" || k == "srcdir")
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())?;
    let real = fs::canonicalize(checkout.join(value)).ok()?;
    (real.is_dir() && real.starts_with(checkout)).then_some(real)
}

/// The names under `<rustup home>/toolchains` that the sandbox can see, and a
/// warning for each entry it cannot: a link that leads outside the rustup
/// home (`rustup toolchain link`) is not bound.
pub(crate) fn installed_toolchains(rustup: &Path) -> (Vec<String>, Vec<String>) {
    let root = fs::canonicalize(rustup).unwrap_or_else(|_| rustup.to_path_buf());
    let mut names = Vec::new();
    let mut warnings = Vec::new();
    let entries =
        fs::read_dir(rustup.join("toolchains")).map(|rd| rd.flatten().collect::<Vec<_>>());
    for entry in entries.unwrap_or_default() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        match fs::canonicalize(entry.path()) {
            Ok(real) if real.starts_with(&root) => names.push(name),
            Ok(_) => warnings.push(format!(
                "toolchain {name} is linked outside the rustup home and is not visible to the sandbox"
            )),
            Err(_) => warnings.push(format!("toolchain {name} is a broken link and is ignored")),
        }
    }
    names.sort();
    (names, warnings)
}

/// `default_toolchain` of `<rustup home>/settings.toml`.
pub(crate) fn host_default(rustup: &Path) -> Option<String> {
    let table: toml::Table = toml::from_str(&read_small(&rustup.join("settings.toml"))?).ok()?;
    table
        .get("default_toolchain")?
        .as_str()
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// What a cargo service's sandbox gets, and what to tell the user about it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CargoSetup {
    pub env: Vec<(String, String)>,
    pub ro_binds: Vec<PathBuf>,
    pub info: Option<String>,
    pub warnings: Vec<String>,
}

/// The services that run cargo.
pub(crate) fn needs_cargo(service: &str) -> bool {
    matches!(service, "cargo_vendor" | "cargo_audit")
}

/// The setup of each of `services`, in order, decided before any of them runs
/// so that a refusal leaves nothing applied. What is chosen is reported (a
/// refusal too, before its error); a service that does not run cargo gets an
/// empty setup. rustup with no toolchain installed, and a rustup home that
/// exists but cannot be used, fail here.
pub(crate) fn cargo_setups(
    services: &[ServiceCall],
    checkout: &Path,
    emit: Emit<'_>,
) -> Result<Vec<CargoSetup>, Error> {
    let mut setups = Vec::with_capacity(services.len());
    for s in services {
        let setup = if needs_cargo(&s.name) && cargo_is_rustup(&CARGO_PATHS) {
            cargo_setup(s, checkout, host_rustup_home())
        } else {
            Ok(CargoSetup::default())
        };
        match setup {
            Ok(setup) => {
                report_setup(&setup, emit);
                setups.push(setup);
            }
            Err((warnings, err)) => {
                report_setup(
                    &CargoSetup {
                        warnings,
                        ..CargoSetup::default()
                    },
                    emit,
                );
                return Err(err);
            }
        }
    }
    Ok(setups)
}

/// The setup of one cargo service for `home`; on failure the warnings to
/// report and the error.
pub(crate) fn cargo_setup(
    s: &ServiceCall,
    checkout: &Path,
    home: RustupHome,
) -> Result<CargoSetup, (Vec<String>, Error)> {
    let rustup = match home {
        RustupHome::Usable(dir) => Some(dir),
        RustupHome::Absent => None,
        RustupHome::Refused(r) => {
            return Err((
                vec![r.to_string()],
                Error::RustupHomeRefused {
                    path: r.path,
                    why: r.why,
                },
            ));
        }
    };
    let (installed, mut warnings) = rustup
        .as_deref()
        .map(installed_toolchains)
        .unwrap_or_default();
    let default = rustup.as_deref().and_then(host_default);
    let pin_file = source_dir(&s.params, checkout).and_then(|dir| {
        PIN_FILES
            .iter()
            .find_map(|name| read_small(&dir.join(name)))
    });
    let choice = select_toolchain(&Inputs {
        pin_file: pin_file.as_deref(),
        installed: &installed,
        host_default: default.as_deref(),
        cargo_is_rustup: true,
    });
    no_toolchain_error(&choice).map_err(|e| (warnings.clone(), e))?;
    let mut setup = CargoSetup::default();
    if let Some(rustup) = rustup {
        setup.env.push((
            "RUSTUP_HOME".to_string(),
            rustup.to_string_lossy().into_owned(),
        ));
        setup.ro_binds.push(rustup);
    }
    setup.env.extend(choice_env(&choice));
    if let Choice::Selected {
        name,
        why,
        warnings: w,
    } = choice
    {
        setup.info = Some(format!("vendoring uses toolchain {name} ({why})"));
        warnings.extend(w);
    }
    setup.warnings = warnings;
    Ok(setup)
}

/// The error for a rustup with no toolchain installed.
pub(crate) fn no_toolchain_error(choice: &Choice) -> Result<(), Error> {
    match choice {
        Choice::NoToolchain => Err(Error::ToolchainUnset {
            tool: "cargo".to_string(),
        }),
        Choice::Selected { .. } | Choice::PlainCargo => Ok(()),
    }
}

/// Tell the reporter what `setup` chose.
pub(crate) fn report_setup(setup: &CargoSetup, emit: Emit<'_>) {
    if let Some(message) = &setup.info {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: message.clone(),
        });
    }
    for message in &setup.warnings {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: message.clone(),
        });
    }
}
