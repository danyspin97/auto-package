//! The proxy checks each request against the shared list, so a host allowed
//! while it runs is seen without a restart.

use super::*;

#[cfg(unix)]
#[test]
fn the_proxy_sees_a_host_allowed_while_it_runs() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    // `localhost` is refused by the host check until it is allowed; once it
    // is, the same proxy gets past the check and refuses the address instead.
    let hosts = std::sync::Arc::new(AllowedHosts::empty());
    let proxy = crate::service_sandbox::Proxy::start(
        std::sync::Arc::clone(&hosts),
        crate::allowed_hosts::Asker::service("test-pkg", "test"),
    )
    .unwrap();
    let ask = || -> String {
        let mut s = UnixStream::connect(proxy.dir().join("proxy.sock")).unwrap();
        s.write_all(b"CONNECT localhost:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while s.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
            line.push(b[0]);
        }
        String::from_utf8_lossy(&line).into_owned()
    };
    assert!(ask().starts_with("HTTP/1.1 403"));
    assert!(proxy.refusals().last().unwrap().contains("is not allowed"));
    hosts
        .allow("localhost", Origin::Run(Via::Tui), None)
        .unwrap();
    assert!(ask().starts_with("HTTP/1.1 403"));
    let last = proxy.refusals().last().cloned().unwrap();
    assert!(last.contains("private or local"), "{last}");
}
