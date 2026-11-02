//! Zig dependency vendoring: a `vendor.tar.zst` for a package whose
//! `build.zig` fetches its dependencies, rebuilt from `build.zig.zon` by a
//! fixed step instead of by an agent. The step runs no zig and no foreign
//! code; it reads the manifest, fetches each dependency through the host
//! allowlist, checks its content hash and writes the archive.

mod fetch;
#[cfg(test)]
mod fetch_archive_tests;
#[cfg(test)]
mod fetch_tests;
mod hash;
#[cfg(test)]
mod hash_tests;
mod tree;
#[cfg(test)]
mod tree_tests;
mod unpack;
#[cfg(test)]
mod unpack_meta_tests;
#[cfg(test)]
mod unpack_tests;
mod zon;
#[cfg(test)]
mod zon_tests;
