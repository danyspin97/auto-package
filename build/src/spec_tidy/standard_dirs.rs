//! The directories a package never needs to own: the ones the `filesystem`
//! package or another base package of every installation already owns.
//!
//! The slice is sorted bytewise (a test checks it) and is looked up by
//! [`is_standard_dir`].
//!
//! How it was built: the directories of `rpm -qlv filesystem` on openSUSE
//! Tumbleweed (20261108), without the per-language trees (`man`, `locale`,
//! `help`, matched by pattern instead), the `/usr/local` and `/usr/etc`
//! trees, the architecture-specific `/usr/<triplet>` trees and the
//! `glibc-hwcaps` directories. The `hicolor` theme root, the shell
//! completion directories (`bash-completion`, `zsh`, `fish`) are added by
//! hand: other base packages own them in every distribution. The FHS roots
//! (`/bin`, `/boot`, `/home`, `/tmp`, ...) and the legacy directories
//! (`/etc/xinetd.d`, `/usr/games`, `/var/db`, ...) come from rpmlint's
//! `STANDARD_DIRS` in `rpmlint/checks/FilesCheck.py`.
//!
//! How to refresh it for another distribution: run the `rpm -qlv filesystem`
//! query there, keep the lines that start with `d`, drop the same trees and
//! sort. A directory missing here only costs a harmless extra `%dir`; a
//! directory listed here that the build root lacks fails the build, so add
//! only what every installation has.

pub const STANDARD_DIRS: &[&str] = &[
    "/bin",
    "/boot",
    "/etc",
    "/etc/X11",
    "/etc/aliases.d",
    "/etc/default",
    "/etc/ld.so.conf.d",
    "/etc/logrotate.d",
    "/etc/modprobe.d",
    "/etc/motd.d",
    "/etc/opt",
    "/etc/permissions.d",
    "/etc/profile.d",
    "/etc/skel",
    "/etc/sysconfig",
    "/etc/sysctl.d",
    "/etc/tmpfiles.d",
    "/etc/xdg",
    "/etc/xdg/autostart",
    "/etc/xinetd.d",
    "/home",
    "/lib",
    "/lib/modules",
    "/lib64",
    "/media",
    "/mnt",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/run/lock",
    "/sbin",
    "/selinux",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
    "/usr/X11R6",
    "/usr/bin",
    "/usr/games",
    "/usr/include",
    "/usr/include/X11",
    "/usr/lib",
    "/usr/lib/X11",
    "/usr/lib/browser-plugins",
    "/usr/lib/environment.d",
    "/usr/lib/firmware",
    "/usr/lib/games",
    "/usr/lib/menu",
    "/usr/lib/modprobe.d",
    "/usr/lib/modules",
    "/usr/lib/motd.d",
    "/usr/lib/pkgconfig",
    "/usr/lib/restricted",
    "/usr/lib/restricted/bin",
    "/usr/lib/sysctl.d",
    "/usr/lib/systemd",
    "/usr/lib/systemd/system",
    "/usr/lib/systemd/system-preset",
    "/usr/lib/systemd/user",
    "/usr/lib/systemd/user-preset",
    "/usr/lib/sysusers.d",
    "/usr/lib/tmpfiles.d",
    "/usr/lib/udev",
    "/usr/lib/udev/rules.d",
    "/usr/lib64",
    "/usr/lib64/browser-plugins",
    "/usr/lib64/games",
    "/usr/lib64/pkgconfig",
    "/usr/libexec",
    "/usr/sbin",
    "/usr/share",
    "/usr/share/appdata",
    "/usr/share/applications",
    "/usr/share/bash-completion",
    "/usr/share/bash-completion/completions",
    "/usr/share/dict",
    "/usr/share/doc",
    "/usr/share/doc/packages",
    "/usr/share/fillup-templates",
    "/usr/share/fish",
    "/usr/share/fish/vendor_completions.d",
    "/usr/share/fish/vendor_conf.d",
    "/usr/share/fish/vendor_functions.d",
    "/usr/share/fonts",
    "/usr/share/games",
    "/usr/share/help",
    "/usr/share/icons",
    "/usr/share/icons/hicolor",
    "/usr/share/info",
    "/usr/share/licenses",
    "/usr/share/locale",
    "/usr/share/man",
    "/usr/share/metainfo",
    "/usr/share/mime",
    "/usr/share/mime/packages",
    "/usr/share/misc",
    "/usr/share/pixmaps",
    "/usr/share/pkgconfig",
    "/usr/share/polkit-1",
    "/usr/share/polkit-1/actions",
    "/usr/share/polkit-1/rules.d",
    "/usr/share/sgml",
    "/usr/share/sgml/docbook",
    "/usr/share/sounds",
    "/usr/share/themes",
    "/usr/share/user-tmpfiles.d",
    "/usr/share/xml",
    "/usr/share/xml/docbook",
    "/usr/share/xsessions",
    "/usr/share/zsh",
    "/usr/share/zsh/site-functions",
    "/usr/src",
    "/usr/src/packages",
    "/usr/tmp",
    "/var",
    "/var/adm",
    "/var/adm/backup",
    "/var/adm/update-messages",
    "/var/adm/update-scripts",
    "/var/cache",
    "/var/crash",
    "/var/db",
    "/var/lib",
    "/var/lib/empty",
    "/var/lib/games",
    "/var/lib/misc",
    "/var/local",
    "/var/log",
    "/var/mail",
    "/var/nis",
    "/var/opt",
    "/var/preserve",
    "/var/spool",
    "/var/tmp",
];

/// Whether `dir` (absolute, no trailing slash) is a standard directory.
pub fn is_standard_dir(dir: &str) -> bool {
    STANDARD_DIRS.binary_search(&dir).is_ok() || is_patterned(dir)
}

/// The per-language and per-size trees: `/usr/share/man/<lang>/man<N>`,
/// `/usr/share/locale/<lang>[/LC_MESSAGES]`, `/usr/share/help/<lang>` and
/// the `hicolor` size directories with their standard categories.
fn is_patterned(dir: &str) -> bool {
    let segs: Vec<&str> = dir.split('/').skip(1).collect();
    let tree = |root: &[&str]| segs.len() > root.len() && segs[..root.len()] == *root;
    let sub = |n: usize| segs.get(n).copied().unwrap_or("");
    if tree(&["usr", "share", "man"]) {
        let is_section = |s: &str| {
            s.strip_prefix("man")
                .is_some_and(|r| r.len() == 1 && r.chars().all(|c| c.is_ascii_alphanumeric()))
        };
        return match segs.len() {
            // `man/man1` or `man/<lang>`, then `man/<lang>/man1`.
            4 => true,
            5 => is_section(sub(4)),
            _ => false,
        };
    }
    if tree(&["usr", "share", "locale"]) {
        return segs.len() == 4 || (segs.len() == 5 && sub(4) == "LC_MESSAGES");
    }
    if tree(&["usr", "share", "help"]) {
        return segs.len() == 4;
    }
    if tree(&["usr", "share", "icons", "hicolor"]) {
        let size = sub(4);
        let is_size = size == "scalable"
            || size == "symbolic"
            || size.split('@').next().is_some_and(|s| {
                s.split_once('x')
                    .is_some_and(|(w, h)| is_digits(w) && is_digits(h))
            });
        return match segs.len() {
            5 => is_size,
            6 => is_size && CATEGORIES.contains(&sub(5)),
            _ => false,
        };
    }
    false
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The icon theme's category directories.
const CATEGORIES: &[&str] = &[
    "actions",
    "animations",
    "apps",
    "categories",
    "devices",
    "emblems",
    "emotes",
    "filesystems",
    "intl",
    "mimetypes",
    "places",
    "status",
    "stock",
];
