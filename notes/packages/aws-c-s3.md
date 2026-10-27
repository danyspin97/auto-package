# aws-c-s3 notes

## Build requirements

- The runtime library subpackage is named after the upstream soversion (lib%{name}<major>_<minor>). When the soversion changes, rename the subpackage to match and update any dependents. For 1.4.x this is the 1.4 name, replacing the 1.3 name. This came from the pre-build review and was not confirmed against the built spec.

Checked against 1.4.1 on openSUSE_Factory, x86_64.
