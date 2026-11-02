//! Zig dependency vendoring: a `vendor.tar.zst` for a package whose
//! `build.zig` fetches its dependencies, rebuilt from `build.zig.zon` by a
//! fixed step instead of by an agent. The step runs no zig and no foreign
//! code; it reads the manifest, fetches each dependency through the host
//! allowlist, checks its content hash and writes the archive.

mod hash;
#[cfg(test)]
mod hash_tests;
mod zon;
#[cfg(test)]
mod zon_tests;
