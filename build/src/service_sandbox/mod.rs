//! Running a checkout's `_service` inside bubblewrap, with the network
//! limited to the source hosts.
//!
//! The services (`tar_scm`, `cargo_vendor`, `go_modules`, ...) fetch and
//! unpack attacker-influenced sources and run build tooling on them. They run
//! here instead of through `osc service ra`, which needs the OBS credentials
//! (it asks the API for the project-wide services, and osc reads the
//! credentials from the desktop keyring over the session bus). Each service is
//! the `/usr/lib/obs/service/<name>` helper, called the way osc calls it, with:
//!
//! - only the checkout bound read-write (its `.osc` read-only), the system
//!   read-only, a private `/tmp` and an empty `HOME`, no session bus, no oscrc;
//! - every namespace unshared, the network included: the only way out is a
//!   loopback listener inside the sandbox (a small Python bridge) that hands
//!   each connection to a unix socket served by [`Proxy`], a CONNECT-only
//!   proxy that allows port 443 on the allowlisted hosts and refuses private
//!   and local addresses. A tool that ignores the proxy variables gets no
//!   network at all.
//!
//! Without a working bubblewrap the services are refused, unless
//! `AUTO_PACKAGE_UNSAFE_SERVICE=1` asks for the old unsandboxed `osc service ra`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::allowed_hosts::{AllowedHosts, Asker};
use crate::osc::Error;
use crate::proc::{self, Captured, Emit};

mod allowlist;
mod proxy;
mod run;
mod sandbox;
#[cfg(test)]
mod tests;

pub(crate) use allowlist::*;
pub(crate) use proxy::*;
pub(crate) use run::*;
pub(crate) use sandbox::*;
