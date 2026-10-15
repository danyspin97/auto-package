# python-opentelemetry-instrumentation-wsgi

## Build requirements
- The package installs under the `opentelemetry` namespace, so `%files` must list the `opentelemetry/instrumentation/wsgi/` path, not a flat top-level module directory.
- Runtime Requires must cover the packages the module imports at runtime, so the installed module can be imported without the build tree.
- BuildRequires must include the test dependencies that `%check` needs to import the package and run its tests. Without them `%check` cannot import the module.
- The exact BuildRequires names were not recorded in the journal; read them from the spec before changing them.

Checked against 0.66b1 on openSUSE_Tumbleweed, x86_64.
