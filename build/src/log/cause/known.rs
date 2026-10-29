use super::*;

/// Modules whose import name is not their PyPI/RPM distribution name.
pub(super) const IMPORT_TO_DIST: &[(&str, &str)] = &[
    ("yaml", "PyYAML"),
    ("PIL", "Pillow"),
    ("cv2", "opencv-python"),
    ("sklearn", "scikit-learn"),
    ("bs4", "beautifulsoup4"),
    ("dateutil", "python-dateutil"),
    ("attr", "attrs"),
    ("attrs", "attrs"),
    ("google", "protobuf"),
    ("jwt", "PyJWT"),
    ("OpenSSL", "pyOpenSSL"),
    ("Crypto", "pycryptodome"),
    ("serial", "pyserial"),
    ("magic", "python-magic"),
    ("dns", "dnspython"),
    ("git", "GitPython"),
    ("docx", "python-docx"),
    ("lxml", "lxml"),
    ("zmq", "pyzmq"),
    ("psycopg2", "psycopg2"),
    ("usb", "pyusb"),
    ("skimage", "scikit-image"),
    ("markdown", "Markdown"),
    ("mock", "mock"),
    ("tomllib", "tomli"),
];

/// Modules removed from the standard library (convention 38's table).
pub(super) const REMOVED_STDLIB: &[&str] = &[
    "distutils",
    "imp",
    "asynchat",
    "asyncore",
    "smtpd",
    "pipes",
    "cgi",
    "cgitb",
    "telnetlib",
    "lib2to3",
    "crypt",
    "nntplib",
    "audioop",
    "aifc",
    "chunk",
    "imghdr",
    "mailcap",
    "msilib",
    "nis",
    "ossaudiodev",
    "sndhdr",
    "spwd",
    "sunau",
    "uu",
    "xdrlib",
];

pub(super) fn normalize(s: &str) -> String {
    s.to_lowercase().replace(['-', '.'], "_")
}

/// The distribution name to put in `%{python_module …}` for an import name.
pub(super) fn requirement_for(module: &str) -> String {
    IMPORT_TO_DIST
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, d)| (*d).to_string())
        .unwrap_or_else(|| module.replace('_', "-"))
}

/// Whether the spec already names `dist` as a (python_module) dependency.
pub(super) fn spec_declares(spec: &str, dist: &str) -> bool {
    let want = normalize(dist);
    spec.lines()
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("BuildRequires:") || l.starts_with("Requires:")
        })
        .any(|l| {
            let value = l.split_once(':').map(|(_, v)| v).unwrap_or("");
            value
                .replace(['%', '{', '}'], " ")
                .split_whitespace()
                .any(|tok| normalize(tok) == want)
        })
}

/// Where the failing import happened: the innermost frame in the unpacked
/// sources, else the innermost frame.
pub(super) fn import_site(frames: &[PythonFrame]) -> Option<String> {
    let frame = frames
        .iter()
        .rev()
        .find(|f| matches!(f.class, PathClass::BuildTree { .. }))
        .or_else(|| frames.last())?;
    let path = match &frame.class {
        PathClass::BuildTree { relative } => relative.as_str(),
        _ => frame.path.as_str(),
    };
    Some(match &frame.function {
        Some(func) => format!("imported at {path}:{} (in {func})", frame.line),
        None => format!("imported at {path}:{}", frame.line),
    })
}

pub(super) static CYTHON_LIMITED_API: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Cython (?<c>3\.\d+) requires the Python Limited API version to be (?<v>3\.\d+) or greater")
        .expect("static pattern")
});

pub(super) static CMAKE_LICENCE_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION|BUILD_AGAINST_ALL_FFMPEG_FROM_S3")
        .expect("static pattern")
});

pub(super) static SPLIT_TREE_SUBMODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"No module named '(?<m>cutlass\.[A-Za-z0-9_.]+)'").expect("static pattern")
});
