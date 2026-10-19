## Build requirements

- The upstream wheel for the build architecture is a prebuilt CUDA component, and one wheel covers all CUDA flavors. The spec keeps ExclusiveArch limited to the architecture of that wheel and installs into sitelib, because the wheel is marked as purelib-rooted.

Checked against 10.4.0.35 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The file list is taken from the wheel's RECORD and names exact paths. It is not a placeholder or a glob.
- Bundled headers are deleted at install time, so they are not packaged. Header entries for CUDA flavors that are not shipped are also removed from the installed RECORD, so it matches the files installed.
- The EULA is excluded from the file list and packaged as a license under the dist-info licenses directory. The package uses the SUSE-NonFree license.
- Spec comments avoid percent-macro syntax, because RPM expands it even inside comments.
- The changelog entry is written by hand when the packaging tool's changelog step cannot reach its signing keyring. This is an environment issue on openSUSE_Tumbleweed, x86_64 only.

Checked against 10.4.0.35 on openSUSE_Tumbleweed, x86_64.
