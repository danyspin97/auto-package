---
name: rust-cargo-packaging
description: Use when packaging a standalone Rust package — a binary or library built entirely with Cargo (a Cargo.toml at the source root, no pyproject.toml at all). Covers cargo-packaging macros, vendored dependency tarballs, %{cargo_build}, manual binary installation, and shell-completion subpackages. Do NOT use this for a Python package whose build backend happens to be maturin — that's the maturin-packaging skill instead. A package with BOTH Cargo and Python components mixes this skill's rules with the main Python packaging conventions.
---

# Rust / Cargo packages

These rules apply ONLY to standalone Rust packages (binaries/libraries built
entirely with Cargo). They do NOT apply to Python packages whose build
backend happens to be maturin — those follow the main Python packaging
conventions (see the maturin-packaging skill).

R1. Rust packages use `cargo-packaging` macros. The mandatory global and
   BuildRequires are:

     %global rustflags '-Clink-arg=-Wl,-z,relro,-z,now'

     BuildRequires:  cargo-packaging
     BuildRequires:  pkgconf
     BuildRequires:  pkgconfig

   Add `BuildRequires: pkgconfig(<lib>)` for every C library the crate links
   against (e.g. `pkgconfig(libpcre2-posix)` for pcre2).

R2. Vendored dependencies are distributed as a second source tarball:

     Source1:        vendor.tar.zst

   Extract both in %prep with:

     %autosetup -p1 -a1

R3. Build with the `%{cargo_build}` macro. Pass optional Cargo features with
   `--features '<feature1> <feature2>'`:

     %build
     %{cargo_build} --features 'pcre2'

   Do NOT add `-j` flags — the macro already uses maximum parallelism.

R4. Install with `%{cargo_install}` (from `cargo-packaging`), which puts the
   crate's binaries into `%{buildroot}%{_bindir}`:

     %install
     %{cargo_install}

   Install by hand only what it does not cover (man pages, completions,
   extra binaries built with other features):

     install -D -m 0755 target/release/<extra-binary> %{buildroot}%{_bindir}/<extra-binary>

R4b. `vendor.tar.zst` comes from the `cargo_vendor` OBS service declared in
   `_service`; you cannot run services. If the checkout has no vendor tarball
   and `_service` does not declare one, report `blocked` naming the
   `cargo_vendor` service as the missing step rather than inventing a Source.

R5. Shell-completion subpackages (bash, zsh, fish): each is `BuildArch: noarch`
   and adds `Supplements: (%{name} and <shell>)` alongside its normal
   `Requires: %{name} = %{version}` — the `Supplements` line makes it
   auto-install whenever both the base package and the shell are already
   present, it does not replace the hard `Requires`. Completions are
   generated during `%build` and installed from the build tree. Example for
   a package that generates completions at build time:

     %package bash-completion
     Summary:        Bash Completion for %{name}
     Group:          System/Shells
     Requires:       %{name} = %{version}
     Requires:       bash-completion
     Supplements:    (%{name} and bash)
     BuildArch:      noarch

R6. Some packages have both Cargo (Rust) and Python components. Mix the
   relevant conventions freely — use `cargo-packaging` for the Rust parts and
   the Python macros (main packaging conventions) for the Python parts.
