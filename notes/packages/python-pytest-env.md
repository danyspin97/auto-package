# python-pytest-env notes

## Build requirements
- hatch-vcs (>= 0.5) is a build dependency. The hatchling backend derives the project version from VCS metadata, so the build needs it. The spec must use the hyphenated distribution name.
- On openSUSE_Tumbleweed only, the pytest and python-dotenv bounds in the upstream pyproject were relaxed, because the distro's current packages fall outside them.
Checked against 1.8.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Deselect the test in the plugin's test_env.py that runs pytest in a subprocess under a cleared os.environ. With the environment cleared, the subprocess cannot see the installed package that %check exposes through the module search path, so it fails with ImportError. Applies to every target whose %check tests an installed copy.
Checked against 1.8.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The sdist has no git metadata, so the version cannot come from tags. The scm version must be supplied explicitly at build time. Applies to every target, because the sdist is git-less everywhere.
Checked against 1.8.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Subprocess test ImportError under a cleared environment: fixed by deselecting that test (see Test changes).
Checked against 1.8.1 on openSUSE_Tumbleweed, x86_64.
