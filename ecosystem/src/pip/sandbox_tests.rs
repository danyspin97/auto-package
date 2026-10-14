//! Tests of sandboxed resolution: the mode policy, and (when bubblewrap works
//! here) a hostile sdist run through a real `pip install --dry-run`.

use super::*;
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

#[test]
fn wheels_only_resolution_excludes_sdists() {
    let args = pip_report_args("httpx==0.28.1", true);
    assert!(args.contains(&"--only-binary=:all:"), "{args:?}");
    // Still before the end-of-options marker.
    let only = args
        .iter()
        .position(|a| *a == "--only-binary=:all:")
        .unwrap();
    assert!(only < args.len() - 2, "{args:?}");
    assert_eq!(args.last(), Some(&"httpx==0.28.1"));
}

#[test]
fn wheels_only_failure_says_how_to_get_sdists_built() {
    let stderr = "ERROR: Could not find a version that satisfies the requirement x\n";
    let err = classify_failure("x", 1, stderr.to_string(), true);
    assert!(matches!(err, Error::NoWheel(_)), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("sdist") && msg.contains("--unsafe-resolve") && msg.contains("bubblewrap"),
        "{msg}"
    );
}

#[test]
fn only_the_unsandboxed_modes_have_a_user_visible_notice() {
    assert_eq!(ResolveMode::Sandboxed.notice(), None);
    let wheels = ResolveMode::WheelsOnly.notice().unwrap();
    assert!(
        wheels.contains("wheels only") && wheels.contains("--unsafe-resolve"),
        "{wheels}"
    );
    assert!(
        wheels.contains("AUTO_PACKAGE_UNSAFE_RESOLVE=1"),
        "the env var must be named: {wheels}"
    );
    let unsafe_mode = ResolveMode::Unsandboxed.notice().unwrap();
    assert!(unsafe_mode.contains("without a sandbox"), "{unsafe_mode}");
}

// --- bubblewrap integration ---------------------------------------------

/// Why the bwrap tests cannot run here, if they cannot.
fn skip_reason() -> Option<&'static str> {
    if !ap_core::sandbox::bwrap_works() {
        return Some("bwrap is missing or unprivileged user namespaces are disabled");
    }
    let has_pip = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|d| d.join("pip").is_file());
    (!has_pip).then_some("pip is not on PATH")
}

macro_rules! require_bwrap {
    () => {
        if let Some(why) = skip_reason() {
            eprintln!("SKIPPED {}: {why}", module_path!());
            return;
        }
    };
}

/// What the hostile backend is told to attack.
struct Probe {
    enabled: bool,
    canary: PathBuf,
    marker: PathBuf,
    ssh_dir: PathBuf,
}

impl Probe {
    fn off() -> Self {
        Probe {
            enabled: false,
            canary: PathBuf::new(),
            marker: PathBuf::new(),
            ssh_dir: PathBuf::new(),
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "canary": self.canary,
            "marker": self.marker,
            "ssh_dir": self.ssh_dir,
        })
    }
}

/// An sdist whose in-tree PEP 517 backend needs nothing from the index. While
/// preparing metadata it tries to touch the host (when `probe` is enabled) and
/// records what happened in the `Summary` field, which pip copies into its
/// report.
fn make_sdist(dir: &Path, name: &str, requires: &[&str], probe: &Probe) -> PathBuf {
    let top = format!("{name}-1.0");
    let requires_lines: Vec<String> = requires
        .iter()
        .map(|r| format!("Requires-Dist: {r}"))
        .collect();
    let backend = format!(
        r#"import json, os

PROBE = json.loads({probe_json})
NAME = {name_json}
REQUIRES = {requires_json}

def _attempt(fn):
    try:
        return "OK:" + str(fn())
    except Exception as e:
        return "ERR:" + type(e).__name__

def _write(path):
    with open(path, "w") as f:
        f.write("x")

def _summary():
    r = {{}}
    if PROBE["enabled"]:
        r["read_home"] = _attempt(lambda: open(PROBE["canary"]).read())
        r["write_outside"] = _attempt(lambda: _write(PROBE["marker"]))
        r["write_usr"] = _attempt(lambda: _write("/usr/pwned"))
        r["write_etc"] = _attempt(lambda: _write("/etc/pwned"))
        r["write_root"] = _attempt(lambda: _write("/pwned"))
        r["write_tmp"] = _attempt(lambda: _write("/tmp/scratch"))
        r["write_home"] = _attempt(lambda: _write(os.path.expanduser("~/scratch")))
        r["list_ssh"] = _attempt(lambda: os.listdir(PROBE["ssh_dir"]))
        r["canary_value"] = os.environ.get("CANARY_SECRET")
        r["env_keys"] = sorted(os.environ)
        r["home"] = os.environ.get("HOME")
    return json.dumps(r, sort_keys=True)

def get_requires_for_build_wheel(config_settings=None):
    return []

def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None):
    d = NAME.replace("-", "_") + "-1.0.dist-info"
    os.makedirs(os.path.join(metadata_directory, d))
    lines = ["Metadata-Version: 2.1", "Name: " + NAME, "Version: 1.0",
             "Summary: " + _summary()] + REQUIRES
    with open(os.path.join(metadata_directory, d, "METADATA"), "w") as f:
        f.write("\n".join(lines) + "\n")
    return d

def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    raise RuntimeError("the fake sdist cannot build a wheel")
"#,
        probe_json = serde_json::to_string(&probe.json().to_string()).unwrap(),
        name_json = serde_json::to_string(name).unwrap(),
        requires_json = serde_json::to_string(&requires_lines).unwrap(),
    );
    let pyproject = "[build-system]\nrequires = []\nbuild-backend = \"backend\"\n\
                     backend-path = [\".\"]\n";
    let pkg_info = format!("Metadata-Version: 2.1\nName: {name}\nVersion: 1.0\n");
    let path = dir.join(format!("{top}.tar.gz"));
    let gz = flate2::write::GzEncoder::new(
        fs::File::create(&path).unwrap(),
        flate2::Compression::fast(),
    );
    let mut tar = tar::Builder::new(gz);
    for (file, body) in [
        ("pyproject.toml", pyproject.to_string()),
        ("backend.py", backend),
        ("PKG-INFO", pkg_info),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("{top}/{file}"), body.as_bytes())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    path
}

/// Serve `dir` as a flat find-links index on 127.0.0.1 until the process
/// ends. Returns the base URL.
fn serve_index(dir: &Path) -> String {
    let files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "gz"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/",
        listener.local_addr().unwrap().port()
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut req = [0u8; 2048];
            let n = std::io::Read::read(&mut stream, &mut req).unwrap_or(0);
            let line = String::from_utf8_lossy(&req[..n]);
            let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, ctype, body) = if target == "/" {
                let links: String = files
                    .iter()
                    .map(|(name, body)| {
                        let sha = hex::encode(Sha256::digest(body));
                        format!("<a href=\"{name}#sha256={sha}\">{name}</a>\n")
                    })
                    .collect();
                (
                    "200 OK",
                    "text/html",
                    format!("<html><body>\n{links}</body></html>").into_bytes(),
                )
            } else if let Some((_, body)) = files
                .iter()
                .find(|(name, _)| target.trim_start_matches('/') == name)
            {
                ("200 OK", "application/gzip", body.clone())
            } else {
                ("404 Not Found", "text/plain", b"not found".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    url
}

/// Run pip for `spec` as `mode` would, against a pip config at `conf`, with
/// `home` as the host's home directory. `extra` is added to the variables the
/// caller's environment offers pip.
fn run_report(
    mode: ResolveMode,
    spec: &str,
    home: &Path,
    conf: &Path,
    extra: &[(&str, &str)],
) -> Result<serde_json::Value, Error> {
    let mut vars: Vec<(OsString, OsString)> = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), home.into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("PIP_CONFIG_FILE".into(), conf.into()),
    ];
    vars.extend(extra.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    let mut cmd = match mode {
        ResolveMode::Sandboxed => sandboxed_command(&["pip"], vars.into_iter(), false)?,
        _ => pip_command(&["pip"], vars.into_iter()),
    };
    cmd.args(pip_report_args(spec, mode == ResolveMode::WheelsOnly));
    let out = run_with_timeout(cmd, Duration::from_secs(180))?;
    if !out.status.success() {
        // Unclassified, so a test failure shows pip's own message.
        return Err(Error::Failed {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn summary_of(report: &serde_json::Value, name: &str) -> serde_json::Value {
    let entry = report["install"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("{name} not in {report}"));
    let summary = entry["metadata"]["summary"].as_str().unwrap();
    serde_json::from_str(summary).unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    marker: PathBuf,
    conf: PathBuf,
}

/// A fake host home with a canary and an ssh key, an index holding a hostile
/// sdist that depends on a plain one, and a pip config that reads only it.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(home.join("canary"), "HOME-CANARY").unwrap();
    fs::write(home.join(".ssh/id_ed25519"), "KEY").unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("pwned");
    let index = root.join("index");
    fs::create_dir_all(&index).unwrap();
    let probe = Probe {
        enabled: true,
        canary: home.join("canary"),
        marker: marker.clone(),
        ssh_dir: home.join(".ssh"),
    };
    make_sdist(&index, "sdist-probe", &["sdist-plain"], &probe);
    make_sdist(&index, "sdist-plain", &[], &Probe::off());
    let url = serve_index(&index);
    let conf = root.join("pip.conf");
    fs::write(
        &conf,
        format!("[global]\nno-index = true\nfind-links = {url}\n"),
    )
    .unwrap();
    World {
        _dir: dir,
        home,
        marker,
        conf,
    }
}

#[test]
fn a_plain_sdist_still_resolves_inside_the_sandbox() {
    require_bwrap!();
    let w = world();
    let report = run_report(ResolveMode::Sandboxed, "sdist-plain", &w.home, &w.conf, &[]).unwrap();
    let names: Vec<_> = report["install"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["metadata"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["sdist-plain"]);
    let hash = &report["install"][0]["download_info"]["archive_info"]["hashes"]["sha256"];
    assert!(hash.as_str().is_some_and(|h| h.len() == 64), "{report}");
}

#[test]
fn a_hostile_backend_sees_nothing_of_the_host_and_writes_nothing() {
    require_bwrap!();
    let w = world();
    let canary = "CANARY-9f3a";
    let report = run_report(
        ResolveMode::Sandboxed,
        "sdist-probe",
        &w.home,
        &w.conf,
        &[("CANARY_SECRET", canary), ("GITHUB_TOKEN", canary)],
    )
    .expect("a hostile sdist must still resolve, only without access");

    // The dependency of the hostile sdist resolved too.
    assert!(
        report["install"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["metadata"]["name"] == "sdist-plain"),
        "{report}"
    );

    let seen = summary_of(&report, "sdist-probe");
    let err = |k: &str| seen[k].as_str().unwrap_or("").starts_with("ERR:");
    // (a) the real home is not there to read.
    assert!(err("read_home"), "home file readable: {seen}");
    assert!(err("list_ssh"), "~/.ssh listable: {seen}");
    // (b) nothing outside scratch space is writable.
    assert!(err("write_outside"), "wrote outside: {seen}");
    assert!(err("write_usr"), "wrote /usr: {seen}");
    assert!(err("write_etc"), "wrote /etc: {seen}");
    assert!(err("write_root"), "wrote /: {seen}");
    assert!(!w.marker.exists(), "marker appeared on the host");
    // (c) the environment holds no host secrets.
    assert!(seen["canary_value"].is_null(), "canary visible: {seen}");
    assert!(
        !seen.to_string().contains(canary),
        "canary leaked into the sandbox: {seen}"
    );
    assert_eq!(seen["home"], pip_sandbox::SANDBOX_HOME);
    // Scratch space is writable, so the backend ran for real.
    assert!(
        !err("write_tmp") && !err("write_home"),
        "no scratch: {seen}"
    );
}

/// The same backend, run unsandboxed, does reach the host: without this the
/// test above could pass because the backend never got that far.
#[test]
fn the_probe_does_reach_the_host_without_the_sandbox() {
    require_bwrap!();
    let w = world();
    let report = run_report(
        ResolveMode::Unsandboxed,
        "sdist-probe",
        &w.home,
        &w.conf,
        &[("CANARY_SECRET", "CANARY-9f3a")],
    )
    .unwrap();
    let seen = summary_of(&report, "sdist-probe");
    assert_eq!(seen["read_home"], "OK:HOME-CANARY", "{seen}");
    assert!(w.marker.exists(), "{seen}");
    // The cleared environment still holds back what is not allowlisted.
    assert!(seen["canary_value"].is_null(), "{seen}");
}

#[test]
fn a_timeout_kills_the_sandboxed_process_tree() {
    require_bwrap!();
    let cmd = sandboxed_command(
        &["sleep", "4242"],
        [("PATH".into(), "/usr/bin:/bin".into())].into_iter(),
        false,
    )
    .unwrap();
    let started = Instant::now();
    let err = run_with_timeout(cmd, Duration::from_millis(500)).unwrap_err();
    assert!(matches!(err, Error::Failed { code: -1, .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
    let alive = || {
        fs::read_dir("/proc").unwrap().flatten().any(|e| {
            fs::read(e.path().join("cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains("sleep\u{0}4242"))
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(), "sleep survived the timeout");
}

#[test]
fn a_missing_alternate_interpreter_is_a_launch_error_not_a_sandbox_error() {
    let err = sandboxed_command(
        &["python3.99", "-m", "pip"],
        [("PATH".into(), "/usr/bin:/bin".into())].into_iter(),
        false,
    );
    // Without bwrap the launch error is about bwrap itself; either way it is
    // a launch error, which the alternate-Python search skips.
    assert!(
        matches!(&err, Err(Error::Launch(e)) if e.kind() == std::io::ErrorKind::NotFound),
        "{:?}",
        err.map(|_| ())
    );
}
