# python-accelerate notes

## Build requirements
- BuildRequires the upstream test dependencies for each Python flavor: torch, huggingface-hub, PyYAML, safetensors and parameterized. The test suite imports them, and Tumbleweed provides them for each flavor.
- BuildRequires the C++ compiler (gcc-c++). The torch inductor tests compile C++ code during %check.
- BuildRequires the Python development headers for each flavor. Torch inductor's compile step includes Python.h and warns when it is missing.
Checked against 1.13.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The examples tests are excluded from %check.
- Three ModelEstimator tests are deselected because they need network access.
- test_convert_to_fp32 is deselected because torch inductor could not find Python.h. The development headers were added in the same change, so this deselection may no longer be needed; this build does not confirm which of the two fixed it.
Checked against 1.13.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The sdist omits the tests/test_configs YAML fixtures. They are fetched as an extra source from the upstream tag of the same version, because the tests read them at runtime.
- The sdist also omits tests/test_samples. It is extracted from the same upstream tag alongside test_configs.
Checked against 1.13.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- FileNotFoundError for the tests/test_configs YAML files: fixed by fetching the fixtures from the upstream tag.
- Five TpuConfigTester failures from a missing test_command_file.sh: fixed by extracting tests/test_samples from the upstream tag.
- Torch inductor warning about a missing Python.h: fixed by adding the Python development headers to BuildRequires.
Checked against 1.13.0 on openSUSE_Tumbleweed, x86_64.
