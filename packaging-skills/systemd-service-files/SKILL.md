---
name: systemd-service-files
description: Use when the package is actually a background daemon/server (not most Python libraries) and ships or needs a .service unit, a dedicated system user/group, or runtime-created directories under /run — a *.service file present in the sdist, a --daemon mode, an existing sibling package doing this, or a daemon that needs to drop privileges to its own uid. Covers Source-file placement, %service_add_*/%service_del_* scriptlets, %sysusers_generate_pre for system users, %tmpfiles_create for runtime directories, and %files. Do not invent any of this for a plain library or CLI tool that has no daemon mode.
---

# Systemd service files

Use the `%service_add_*`/`%service_del_*` scriptlet macros — never hand-write
`%systemd_post`/raw `systemctl` calls in %post/%postun. `python-Glances` is
the reference shape:

- The macros need the systemd RPM macros at build time and the ordering
  dependency at run time, in the package that owns the unit:
    BuildRequires:  pkgconfig(systemd)
    %{?systemd_ordering}
- The unit file arrives as its own numbered source, not out of the
  tarball: `Source2: <name>.service` (and, if the package also ships a
  firewalld rule, `Source3: <name>.firewalld`).
- Install it in %install:
    mkdir -p %{buildroot}%{_unitdir}
    install -D -m 644 %{SOURCE2} %{buildroot}%{_unitdir}/
- Scriptlets, on whichever (sub)package owns the unit file (Glances puts
  the whole systemd/firewalld piece in a `-common` subpackage separate
  from the main noarch Python package):
    %pre
    %service_add_pre <name>.service
    %post
    %service_add_post <name>.service
    %preun
    %service_del_preun <name>.service
    %postun
    %service_del_postun <name>.service
- %files: `%{_unitdir}/<name>.service`.

Only add this when %check/build.log actually shows the package is a
service (a `*.service` file in the sdist, a `--daemon` mode, an existing
sibling package doing this) — do not invent a unit file for a plain
library or CLI tool.

## A daemon that runs as its own dedicated user needs `sysusers.d`, not `useradd`

Any daemon package that needs a dedicated system user (very common for
systemd services — the same population this skill already targets) uses the
modern systemd-sysusers mechanism instead of a hand-rolled `useradd`/`groupadd`
call in `%pre`. This is a real, current openSUSE idiom, extremely common
across Base:System daemon packages (confirmed independently across many
packages including `rtkit`, `systemd-netlogd`, `dbus-1`, `at`, `nscd`,
`multipath-tools`, `nfs-utils`, `polkit`, `rsyslog`). Reference shape
(`rtkit`/`systemd-netlogd`):

    BuildRequires:  sysuser-tools
    %sysusers_requires

    %build
    %sysusers_generate_pre %{SOURCE1} <name> <name>.conf

    %install
    install -Dm 0644 %{SOURCE1} %{buildroot}%{_sysusersdir}/<name>.conf

    %pre -f <name>.pre
    %service_add_pre <name>.service

    %files
    %{_sysusersdir}/<name>.conf

- `Source1: <name>.sysusers` is a one-line sysusers.d declaration, e.g.
  `u <name> - "description"` for a bare system user, or
  `u <name> -:<parent-group> "GECOS"` to also create/join a group.
- `%sysusers_generate_pre` (run in `%build`) generates a `<name>.pre`
  scriptlet fragment on the fly; `%pre -f <name>.pre` includes it so the
  user/group exists before `%post` runs — do not write the `useradd` logic
  yourself.
- If the daemon needs to belong to an existing group (not one it creates),
  declare that as a virtual dependency, not a package name:
  `Requires(pre): group(<groupname>)`.
- `%{?sysusers_requires}` (note vs. `%sysusers_requires` above — check which
  your target macro set defines; both forms appear in real specs) pulls in
  whatever provides the sysusers.d mechanism at runtime.

## Runtime-created directories/files under `/run` need `tmpfiles.d`

The `sysusers.d` companion mechanism for directory/file lifecycle instead of
users: a daemon that expects `/run/<name>/` (or similar) to exist declares it
via `tmpfiles.d` rather than `mkdir`-ing it at daemon startup or trying to
own it as a normal packaged directory (which would fail since `/run` is a
tmpfs, recreated empty every boot). Reference shape (`syslogd`):

    BuildRequires:  rpm_macro(_tmpfilesdir)
    Source10:       <name>.tmpfiles

    %install
    install -Dm 0644 %{SOURCE10} %{buildroot}%{_tmpfilesdir}/<name>.conf

    %post
    %tmpfiles_create %{_tmpfilesdir}/<name>.conf

    %files
    %{_tmpfilesdir}/<name>.conf

`%tmpfiles_create` belongs in `%post` (not `%pre`) so the directory exists
immediately after install without waiting for the next boot. The `.tmpfiles`
source file itself uses the systemd-tmpfiles.d(5) line format (e.g.
`d /run/<name> 0755 <user> <group> -`).

## Runtime-created files in `%files`

A PID file or socket under `/run` (never `/var/run`, a compatibility symlink)
is created by the daemon at run time on a tmpfs: do not package it, not even
as `%ghost`; its directory comes from `tmpfiles.d` above. A state file the
daemon creates under `/var/lib/<name>/` may be `%ghost`-listed so rpm owns
the path:

    %dir %{_localstatedir}/lib/<name>
    %ghost %{_localstatedir}/lib/<name>/<name>.db
