---
name: npm-vendor-packaging
description: Use when a Python sdist's build (typically a hatch/setuptools build hook such as hatch_build.py) runs npm or node to build a JavaScript frontend and the build fails offline with "npm: command not found" or a network error, AND the sdist ships a package-lock.json. Covers BuildRequires nodejs/npm, vendoring the npm dependencies with the `node_modules` OBS service, an offline `npm ci` against local-npm-registry, and making the build hook reuse the prebuilt frontend. Try this BEFORE switching to the upstream wheel (use_upstream_wheel), which stays the last resort.
---

# npm-based sdists: vendored node_modules

OBS builds have no network. When an sdist's build hook runs `npm install` /
`npm run build` (open-webui's `hatch_build.py` building its SvelteKit
frontend is the standing case), the build dies with `npm: command not found`
or an `ENOTFOUND registry.npmjs.org`. The fix is to vendor the npm
dependencies with the `node_modules` OBS service, build the frontend
yourself in `%build`, and stop the hook from running npm. Only if that
attempt fails for a reason you cannot fix, return `use_upstream_wheel`
(the prebuilt-wheel route is the last resort, not the first reaction).

N1. Preconditions. The sdist (or repo archive) must contain a
   `package-lock.json` (`lockfileVersion` 2 or higher). The service reads
   only `package-lock.json`; a project with only `pnpm-lock.yaml` or
   `yarn.lock` cannot use it, so report `use_upstream_wheel` for those and
   say the lock file type in `why`. Find the lock file and the frontend
   directory with `find source -name package-lock.json -not -path '*/node_modules/*'`
   and read the build hook to see which `npm` commands it runs and where the
   output goes (`build/`, `dist/`, a copy into the package directory).

N2. BuildRequires:

     BuildRequires:  nodejs-devel
     BuildRequires:  npm
     BuildRequires:  local-npm-registry

   Use `zypper search nodejs` / `zypper search npm` to find the unversioned
   or versioned names that exist (`nodejs22`, `npm22`, ...), and pick the
   major version the project's `engines` field in `package.json` allows.
   Add `export NODE_OPTIONS=--max-old-space-size=4096` in `%build` if the
   bundler (vite, webpack) runs out of memory.

N3. `_service`. You cannot run services; the pipeline runs them in manual
   mode. Add the `node_modules` service next to the existing ones, keeping
   the services already there:

     <service name="node_modules" mode="manual">
       <param name="input">package-lock.json</param>
     </service>

   Allowed parameters: `input` (lock file, default `package-lock.json`),
   `node-dir` (directory the per-dependency tarballs are written to, default
   `node_modules`), `omit-optional` (skip optional dependencies),
   `legacy-container`, `cpio`, `output`, `source-offset` (legacy cpio mode;
   avoid it). Paths must be relative and stay inside the checkout. The
   service downloads every tarball named by the lock file into `node_modules/`.
   `package-lock.json` itself must sit next to the spec as its own Source;
   copy it out of the sdist into the checkout. The pipeline does both file steps
   itself when it takes this route: it copies the sdist's `package-lock.json` into the
   checkout and writes the `node_modules` entry into `_service`; only the spec
   wiring (N4, N5) is left to you. If a copy is still missing, ask for it with
   `stage_files` (`from` `source/<path>/package-lock.json`, `to` `package-lock.json`).

N4. Spec sources:

     Source10:       package-lock.json
     #!CreateArchive
     Source11:       node_modules.tar.gz

   `#!CreateArchive` makes OBS pack the `node_modules/` directory into the
   named archive at build time. If a `node_modules.spec.inc` is generated,
   `%include` it rather than hand-writing Source lines.

N5. Offline install in `%prep` (or the start of `%build`), from the vendored
   tarballs through the local registry, using the lock file from Source10 and
   the `package.json` already in the sdist:

     cp %{SOURCE10} <frontend-dir>/package-lock.json
     cd <frontend-dir>
     local-npm-registry %{_sourcedir}/node_modules install --also=dev

   `local-npm-registry` serves the tarballs on localhost and runs the
   install against them. If the project's own scripts insist on
   `npm ci`, run it after pointing npm at the same registry:
   `npm ci --offline --ignore-scripts` plus `npm config set cache` to a
   directory the registry populated. Prefer `--ignore-scripts` and run the
   needed scripts (`npm rebuild <pkg>`) explicitly; postinstall scripts that
   download binaries (puppeteer, playwright, esbuild platform packages)
   cannot work offline and should be skipped with env vars
   (`PUPPETEER_SKIP_DOWNLOAD=1`, `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`).

N6. Build the frontend yourself in `%build`, BEFORE the Python wheel build:

     cd <frontend-dir>
     npm run build
     cd -
     %pyproject_wheel

   Use exactly the script the hook would have run (`npm run build`,
   `npm run build:prod`, ...). Disable telemetry and update checks
   (`export CI=true NEXT_TELEMETRY_DISABLED=1 DO_NOT_TRACK=1`), since they
   try the network.

N7. Make the build hook skip npm and reuse the prebuilt output. Read the
   hook (`hatch_build.py`, `setup.py` custom command, `build.py`) and write a
   quilt patch (see the patch conventions in the main prompt; never edit
   `source/` in place without `quilt`) that does the least change needed:
   - if the hook has an env-var or "already built" guard, set it in `%build`
     instead of patching;
   - otherwise make the `npm install` / `npm run build` calls conditional on
     the output directory not existing, so that the output built in N6 is
     picked up and copied into the package as the hook already does;
   - never delete the hook's copy step: the built frontend must still end up
     inside the installed package (check `%files`, e.g.
     `%{python_sitelib}/<pkg>/frontend/`).
   Patch comments should say the patch only disables the network step.

N8. `%check` / verification: confirm in `%install` output that the built
   assets exist in the package (`test -d %{buildroot}%{python_sitelib}/<pkg>/frontend`
   or whatever the hook copies to), then run the normal import smoke test.
   Run `%fdupes` as usual; bundled JS in `site-packages` is expected, but
   `rpmlint` complaints about `node_modules` leaking into the package mean the
   hook copied the wrong directory.

N9. When to stop and use the wheel: the lock file is not `package-lock.json`;
   the build needs a toolchain that is not in OBS (e.g. a Rust/wasm
   toolchain for a JS dependency, a specific Node major that does not exist
   in the distro); or the `node_modules` attempt failed twice for reasons
   you cannot fix in the spec. Then return `use_upstream_wheel` with `why`
   quoting the failing line, and load `prebuilt-wheel-packaging` for the
   next turn.
