// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `Host`-header guard: the DNS-rebinding defense `stella-observatory`
//! already had and this crate did not (#1130).
//!
//! # What rebinding actually is
//!
//! A server that trusts its bind address alone is reachable from a browser it
//! never intended to serve. An attacker's page is loaded from
//! `evil.example`, whose DNS record has a one-second TTL; once the page is
//! running, the record is re-pointed at `127.0.0.1`. The browser's
//! same-origin check already passed — the origin is still `evil.example` — so
//! the page may now issue requests that land on the loopback service. The bind
//! address refused nothing, because the connection genuinely arrives on
//! loopback.
//!
//! The `Host` header is what survives that: the browser sends the *name* it
//! believes it is talking to, so a rebound request announces `evil.example`
//! while a legitimate one announces `localhost` or `127.0.0.1`. Comparing it
//! against what this server expects to be called is the whole defense.
//!
//! Bearer auth (every route but `/healthz`) already blunts the attack — the
//! page cannot read the token, so it cannot get past a 401 — which is why this
//! is defense in depth rather than the only thing standing between a browser
//! and the engine. It is still worth having: the sibling service in this same
//! repo has it, and an unauthenticated `/healthz` is reachable without it.
//!
//! # Why the policy depends on the bind
//!
//! `stella-serve` binds `127.0.0.1` for local use and `0.0.0.0:$PORT` inside a
//! container. Those are different threat models and a single hardcoded
//! loopback check serves only the first:
//!
//! - **Loopback bind** — the rebinding case exactly. The expected `Host` is a
//!   loopback name or address, and anything else is refused.
//! - **Non-loopback bind with an allow-list** — the operator has said what
//!   hostnames this deployment answers to, so those are accepted (alongside
//!   loopback, which is where a container's own liveness probe comes from).
//! - **Non-loopback bind with no allow-list** — there is no way to know what
//!   this deployment is legitimately called, and inventing one would refuse
//!   every real request. The guard reports itself [`HostMode::Inert`] at
//!   startup rather than pretending to enforce.
//!
//! That last arm is a deliberate degrade-open, and it is why
//! [`HostPolicy::mode`] is emitted on the `Listening` event: a guard that is
//! off is only acceptable when an operator can see that it is off.
//!
//! # A missing `Host` is allowed
//!
//! Same posture as `stella-observatory::host_is_local`, and for the same
//! reason: a browser `fetch` always sends one, so its absence means the
//! request did not come from the attack this guards against. Refusing it would
//! break raw `curl`/HTTP-1.0 clients to defend against nothing.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

/// What the `Host` guard is enforcing, reported at startup so an operator can
/// see which arm their configuration landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostMode {
    /// Loopback bind: only loopback names/addresses (plus any configured
    /// allow-list entry) are accepted.
    Loopback,
    /// Non-loopback bind with a configured allow-list: that list, plus
    /// loopback and the bound address's own literal.
    AllowList,
    /// Non-loopback bind with no allow-list — nothing is refused. Named, not
    /// silent: see the module docs.
    Inert,
}

/// The compiled `Host` policy for one bound server.
#[derive(Debug, Clone)]
pub(crate) struct HostPolicy {
    mode: HostMode,
    /// Operator-supplied hostnames, already normalized by [`normalize`].
    allowed: Vec<String>,
    /// The bound address's own literal, accepted under [`HostMode::AllowList`]
    /// so a probe addressing the container by IP is not refused. `None` for an
    /// unspecified bind (`0.0.0.0`), which is never a meaningful `Host`.
    bound_ip: Option<IpAddr>,
}

impl HostPolicy {
    /// Compile the policy for a bound address and an operator allow-list.
    ///
    /// Takes the **bound** address rather than the configured one, so a
    /// `127.0.0.1:0` test bind is judged on the address it actually got.
    pub(crate) fn new(bound: SocketAddr, allowed_hosts: &[String]) -> Self {
        let allowed: Vec<String> = allowed_hosts
            .iter()
            .map(|h| normalize(h))
            .filter(|h| !h.is_empty())
            .collect();
        let ip = bound.ip();
        let mode = if ip.is_loopback() {
            HostMode::Loopback
        } else if allowed.is_empty() {
            HostMode::Inert
        } else {
            HostMode::AllowList
        };
        Self {
            mode,
            allowed,
            bound_ip: (!ip.is_unspecified()).then_some(ip),
        }
    }

    /// Which arm this policy landed in — reported on `Listening`.
    pub(crate) fn mode(&self) -> HostMode {
        self.mode
    }

    /// Whether a request carrying this `Host` header may proceed to routing.
    ///
    /// `None` (no header at all) is permitted — see the module docs.
    pub(crate) fn permits(&self, host_header: Option<&str>) -> bool {
        let Some(raw) = host_header else {
            return true;
        };
        if self.mode == HostMode::Inert {
            return true;
        }
        let host = normalize(raw);
        if host.is_empty() {
            // A present-but-empty `Host` is not the absent case: something
            // sent the header and left it blank, which matches no expected
            // identity. Refuse rather than read it as "not a browser".
            return false;
        }
        if self.allowed.contains(&host) {
            return true;
        }
        // Parse, never prefix-match. `127.0.0.1.attacker.example` is a
        // registrable name that satisfies a `starts_with("127.")` test, and
        // that near-miss is the whole reason this check is worth writing
        // carefully — same rule `stella-observatory::host_is_local` applies.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return ip.is_loopback() || self.bound_ip.is_some_and(|bound| bound == ip);
        }
        // `localhost` stays an explicit arm because it is a name, not an
        // address, so it never reaches the parse above.
        host == "localhost"
    }
}

/// Reduce a `Host` header value to the identity it names: no port, no
/// brackets, lowercased, no root-zone dot.
///
/// Every one of those is a bypass if skipped. `EVIL.EXAMPLE` beats a
/// case-sensitive compare; `evil.example.` is the same FQDN with the root
/// label spelled out and beats an exact-string compare; a `:port` suffix means
/// the same host on a different port compares unequal, which turns the
/// allow-list into a port allow-list by accident.
fn normalize(host: &str) -> String {
    let host = host.trim();
    // Bracketed IPv6 literal (`[::1]:8080`) — the brackets are required by
    // RFC 3986 precisely so the colons inside cannot be read as a port.
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        // Only split a *trailing* `:port`. An unbracketed bare IPv6 literal is
        // malformed in a `Host` header, and losing its last group makes it
        // fail to parse as an address — which refuses it, the safe direction.
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    hostname.trim_end_matches('.').trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback() -> HostPolicy {
        HostPolicy::new("127.0.0.1:8080".parse().unwrap(), &[])
    }

    fn container(allowed: &[&str]) -> HostPolicy {
        let allowed: Vec<String> = allowed.iter().map(|s| (*s).to_string()).collect();
        HostPolicy::new("0.0.0.0:8080".parse().unwrap(), &allowed)
    }

    #[test]
    fn a_loopback_bind_enforces_loopback_identities() {
        let policy = loopback();
        assert_eq!(policy.mode(), HostMode::Loopback);
        for ok in [
            "localhost",
            "localhost:8080",
            "LOCALHOST",
            "127.0.0.1",
            "127.0.0.1:8080",
            "127.5.5.5",
            "[::1]",
            "[::1]:8080",
        ] {
            assert!(policy.permits(Some(ok)), "{ok} must be permitted");
        }
    }

    /// The #1130 acceptance on the loopback configuration: a rebound request
    /// announces the attacker's name, and that is what gets refused.
    #[test]
    fn a_loopback_bind_refuses_a_rebound_host() {
        let policy = loopback();
        for bad in [
            "evil.example",
            "evil.example:8080",
            // The near-miss the parse-don't-prefix-match rule exists for: a
            // registrable name an attacker can own, which a `starts_with`
            // check would wave through.
            "127.0.0.1.attacker.example",
            "localhost.attacker.example",
            // Trailing-dot FQDN spelling of the same attacker name.
            "evil.example.",
            // Uppercased, to prove the compare is not case-sensitive.
            "EVIL.EXAMPLE",
            // A non-loopback address is not this server's identity either.
            "10.0.0.5",
            "",
        ] {
            assert!(!policy.permits(Some(bad)), "{bad} must be refused");
        }
    }

    /// A missing header is not the attack — a browser always sends one. Same
    /// posture as `stella-observatory::host_is_local`.
    #[test]
    fn an_absent_host_is_permitted_on_every_mode() {
        assert!(loopback().permits(None));
        assert!(container(&["engine.internal"]).permits(None));
        assert!(container(&[]).permits(None));
    }

    /// The container case the issue calls out: a `0.0.0.0` bind cannot use a
    /// hardcoded loopback check, so the operator names what the deployment
    /// answers to.
    #[test]
    fn a_container_bind_honors_its_allow_list_and_refuses_everything_else() {
        let policy = container(&["engine.internal", "Engine.Example.Com."]);
        assert_eq!(policy.mode(), HostMode::AllowList);
        assert!(policy.permits(Some("engine.internal")));
        assert!(policy.permits(Some("engine.internal:8080")));
        // The allow-list is normalized on the way in, so an operator writing
        // it with mixed case or a trailing dot still matches the wire form.
        assert!(policy.permits(Some("engine.example.com")));
        // Loopback stays permitted: a container's own liveness probe is a
        // legitimate caller and must not need an allow-list entry.
        assert!(policy.permits(Some("127.0.0.1")));
        assert!(policy.permits(Some("localhost")));

        assert!(!policy.permits(Some("evil.example")));
        assert!(!policy.permits(Some("engine.internal.attacker.example")));
    }

    /// A specific non-loopback bind accepts its own literal, so addressing the
    /// container by the IP it is actually on is never refused.
    #[test]
    fn a_bound_literal_is_accepted_but_a_different_address_is_not() {
        let policy = HostPolicy::new(
            "10.1.2.3:8080".parse().unwrap(),
            &["engine.internal".to_string()],
        );
        assert!(policy.permits(Some("10.1.2.3")));
        assert!(policy.permits(Some("10.1.2.3:8080")));
        assert!(!policy.permits(Some("10.1.2.4")));
    }

    /// `0.0.0.0` is a bind directive, not an identity — it must never become
    /// an accepted `Host`, or "listen everywhere" would read as "answer to
    /// anything".
    #[test]
    fn an_unspecified_bind_is_not_itself_an_accepted_identity() {
        let policy = container(&["engine.internal"]);
        assert!(!policy.permits(Some("0.0.0.0")));
    }

    /// No allow-list on a non-loopback bind: the guard cannot know what the
    /// deployment is called, so it refuses nothing — and says so, rather than
    /// leaving an operator to believe a check is running.
    #[test]
    fn a_container_bind_without_an_allow_list_is_inert_and_named() {
        let policy = container(&[]);
        assert_eq!(policy.mode(), HostMode::Inert);
        assert!(policy.permits(Some("anything.at.all")));
        assert!(policy.permits(Some("")));
    }

    #[test]
    fn normalize_strips_port_brackets_case_and_the_root_dot() {
        assert_eq!(normalize("Example.COM:8443"), "example.com");
        assert_eq!(normalize("example.com."), "example.com");
        assert_eq!(normalize("[2001:db8::1]:8080"), "2001:db8::1");
        assert_eq!(normalize("  localhost  "), "localhost");
    }
}
