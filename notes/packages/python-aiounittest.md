# python-aiounittest build notes

## Build requirements
- The source has no [build-system] table: pyproject.toml holds only [tool.pytest.ini_options] and setup.cfg only an [egg_info] section. The spec must supply the build backend and pytest itself.
Checked against 1.5.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The test module that overrides get_event_loop calls asyncio.get_event_loop() with no current loop. On Python 3.14 that raises RuntimeError, and it broke collection because the library's default get_event_loop is called at collection time. The test was ported off asyncio.get_event_loop() so collection succeeds.
Checked against 1.5.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Python 3.14: pytest collection failed with "There is no current event loop" from the get_event_loop override in tests/test_asynctestcase_get_event_loop.py. Fixed by porting that test off asyncio.get_event_loop().
Checked against 1.5.0 on openSUSE_Tumbleweed, x86_64.
