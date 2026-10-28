//! Outbound HTTP agents that only reach public hosts over https.
//!
//! The URLs fetched come from upstream metadata, so a redirect or a DNS answer
//! could point at the loopback interface, the local network or a cloud
//! metadata endpoint. The check sits in the resolver, which ureq consults for
//! every connection including each redirect hop, and it applies to the address
//! actually connected to, so a name that resolves differently later is covered.
//!
//! Behind a proxy the proxy resolves the target, so only literal IP targets
//! can be refused (see [`LiteralIpGuard`]); a name that resolves to a private
//! address is the proxy's to deal with.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

mod limiter;
mod policy;
mod retry;

#[cfg(test)]
mod tests;

pub use limiter::*;
pub use policy::*;
pub use retry::*;
