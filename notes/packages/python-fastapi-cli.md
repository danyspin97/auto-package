# python-fastapi-cli

## Build requirements
- The generator dropped all runtime Requires. The package's dependencies (pydantic, rich, fastapi, httpx, typer, uvicorn, rich-toolkit) were added as BuildRequires and Requires, because both the import and the test suite need them.
- tomli is needed only on Python older than 3.11, where the standard library has no tomllib.
- uvicorn's standard extras are Requires. The other optional extras are Recommends.
- The package has no console scripts, so no alternatives block is needed.
- Checked against 0.0.32 on openSUSE_Tumbleweed, x86_64. The build succeeded with the BuildRequires above. The Requires and Recommends come from pre-build review and were not separately verified.
