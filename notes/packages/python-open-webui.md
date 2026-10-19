# python-open-webui notes

## Build requirements
- Runtime Requires (python-base, python-typer, python-accelerate and the rest of the list) are declared explicitly in the spec. Why: the wheel-based spec has to carry them itself.
- The vendored npm route needs the node_modules service helper installed on the build host. Without it the node_modules archive cannot be created. This is a host requirement, not a BuildRequires, and the upstream wheel route does not need it.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Upstream wheel route (used): build from the pure-Python upstream wheel for the release, pinned by checksum, instead of the sdist. Why: the wheel already contains the built frontend, so no npm install or network build is needed. It is architecture-independent, so it serves every target.
- Vendored npm route (not used): package-lock.json from the sdist plus a node_modules archive would let the frontend build offline. Why: the sdist's build hook runs npm install and a build over the network. Needs the host helper above.
- Wheel spec: License is set, license files come from the sdist, and %files is filled in. The rpmlint filters target paths under site-packages/open_webui/ and open_webui/frontend/, which match the wheel layout.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The checkout had no node_modules archive or service entry for it, and package-lock.json could not be copied in → switched to the upstream wheel route.
- The vendored route failed because the node_modules service helper is not installed on the build host → wheel route, as above.
- Wheel spec still had placeholders (License FIXME, TODO %files) → license set and %files filled.
- Installed-but-unpackaged data/readme.txt and requirements-slim.txt in site-packages for python3.13 and python3.14 → removed in the spec.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.
