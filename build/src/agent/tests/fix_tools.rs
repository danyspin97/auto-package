//! Tests for the web grants of a FixBug turn.

use super::*;
use crate::agent::fix_tools::web_grants;
use crate::allowed_hosts::{AllowedHosts, Origin};

fn hosts(grants: &str) -> Vec<&str> {
    grants
        .split(',')
        .filter_map(|g| g.strip_prefix("WebFetch(domain:")?.strip_suffix(')'))
        .collect()
}

#[test]
fn a_bug_fix_turn_has_no_web_grant_of_its_own_and_never_searches_the_web() {
    let ws = std::path::Path::new("/srv/ws");
    let co = Some(std::path::Path::new("/srv/ws/checkout"));
    let tools = turn_allowed_tools(Task::FixBug, ws, co);
    // Besides `pypi.org`, which every checkout turn has, each web grant comes
    // from `web_grants`: none is unconditional.
    assert_eq!(tools.matches("WebFetch").count(), 1, "{tools}");
    assert!(tools.contains("WebFetch(domain:pypi.org)"), "{tools}");
    assert!(!tools.contains("WebSearch"), "{tools}");
    assert!(tools.contains("quilt push *"), "{tools}");
    // The dependency patch turn keeps its GitHub reading.
    let dep = turn_allowed_tools(Task::PatchDependency, ws, co);
    assert!(dep.contains("api.github.com"), "{dep}");
}

#[test]
fn with_an_empty_allowed_hosts_file_these_hosts_are_reachable_and_no_others() {
    let got = web_grants(
        &AllowedHosts::empty(),
        &["codeberg.org".into(), "git.example.org".into()],
    );
    let mut granted = hosts(&got[1..]);
    granted.sort();
    assert_eq!(
        granted,
        [
            "api.github.com",
            "codeberg.org",
            "github.com",
            "lore.kernel.org",
            "raw.githubusercontent.com",
            "salsa.debian.org"
        ]
    );
}

#[test]
fn only_hosts_the_user_allowed_or_the_tool_trusts_are_granted() {
    let none = AllowedHosts::empty();
    let fetch = ["codeberg.org".to_string(), "git.example.org".to_string()];
    let got = web_grants(&none, &fetch);
    let granted = hosts(&got[1..]);
    // Fixed forges and kernel.org's hosts are trusted; the rest is refused.
    assert!(granted.contains(&"codeberg.org"), "{got}");
    assert!(granted.contains(&"salsa.debian.org"), "{got}");
    assert!(granted.contains(&"lore.kernel.org"), "{got}");
    assert!(!granted.contains(&"git.example.org"), "{got}");
    assert!(!granted.contains(&"src.fedoraproject.org"), "{got}");
    assert!(!granted.contains(&"gitweb.gentoo.org"), "{got}");
    // Allowing a host grants it, once.
    none.allow("src.fedoraproject.org", Origin::Env, None)
        .unwrap();
    none.allow("git.example.org", Origin::Env, None).unwrap();
    let got = web_grants(&none, &fetch);
    let granted = hosts(&got[1..]);
    assert!(granted.contains(&"src.fedoraproject.org"), "{got}");
    assert!(granted.contains(&"git.example.org"), "{got}");
    assert_eq!(granted.iter().filter(|h| **h == "codeberg.org").count(), 1);
    assert!(web_grants(&none, &[]).starts_with(",WebFetch"));
}

#[test]
fn the_grants_never_name_a_search_or_the_vendor_page() {
    let got = web_grants(&AllowedHosts::empty(), &[]);
    assert!(
        !got.contains("WebSearch") && !got.contains("suse.com"),
        "{got}"
    );
}
