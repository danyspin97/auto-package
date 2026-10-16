# python-fastapi-cli

## Build requirements
- The generator dropped all runtime Requires. The package's dependencies (pydantic, rich, fastapi, httpx, typer, uvicorn, rich-toolkit) were added as BuildRequires and Requires, because both the import and the test suite need them.
- tomli is needed only on Python older than 3.11, where the standard library has no tomllib.
- uvicorn's standard extras are Requires. The other optional extras are Recommends.
- The package has no console scripts, so no alternatives block is needed.
- The coverage module is a BuildRequires because the test that runs the script goes through it.
- fastapi-cloud-cli 0.1.1 or newer is a BuildRequires, not a Requires: the CLI imports it optionally, and the version test needs it importable.
- Checked against 0.0.32 on openSUSE_Tumbleweed, x86_64. The build succeeded with these BuildRequires. The Requires and Recommends come from pre-build review and were not separately verified.

## Failure history
- The script test failed without the coverage module; fixed by adding coverage as a BuildRequires. Checked against 0.0.32 on openSUSE_Tumbleweed, x86_64.
- The version test failed without fastapi-cloud-cli importable; fixed by adding fastapi-cloud-cli 0.1.1 or newer as a BuildRequires. Checked against 0.0.32 on openSUSE_Tumbleweed, x86_64.
