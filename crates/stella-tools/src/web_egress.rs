//! Where a model-chosen fetch is allowed to land — the egress denylist for
//! the web family (#939, the ruling on #615).
//!
//! The agent decides what to fetch, and that decision is shaped by content it
//! reads: a page it was asked to summarise, a README, an issue body, a CI log.
//! Prompt injection is the normal condition for a tool that fetches, not an
//! edge case, so the destination set is bounded by default: loopback, RFC1918,
//! link-local (the cloud instance-metadata endpoints), unique-local,
//! carrier-grade NAT, and the `localhost` / `.internal` / `.local` name
//! families are refused unless an operator names them in
//! `[egress] allow` in `~/.stella/web_auth.toml`.
//!
//! Two mechanisms, because one cannot cover both halves of the problem:
//!
//!   * [`EgressPolicy::check_url`] runs on the URL — the one the tool was
//!     handed, and again on every redirect hop. It is the ONLY check that can
//!     see a literal-IP destination at all, because hyper-util skips the
//!     resolver entirely when the host parses as an address
//!     (`connect/http.rs`: "if the host is already an IP addr … skip resolving
//!     the dns"). It is also the only place a port fence can be enforced.
//!   * [`GuardedResolver`] runs after DNS, and *filters* the answer rather
//!     than merely inspecting it: the address set hyper connects to is the set
//!     this policy approved, and there is no second resolution to race. That
//!     is what closes DNS rebinding, and it is why a hostname under attacker
//!     control cannot launder `127.0.0.1` through a public name.
//!
//! Checking only the name would be defeated by a public name resolving to a
//! private address; checking only the original URL would be defeated by a
//! public URL that 302s to the metadata endpoint. Hence both.
//!
//! Two residuals, recorded rather than discovered:
//!
//!   * **Proxies.** With `HTTP_PROXY`/`HTTPS_PROXY` set, reqwest resolves and
//!     connects to the *proxy*; the real destination travels in the `CONNECT`
//!     line and never reaches [`GuardedResolver`]. `check_url` still refuses
//!     literal-IP and denied-name targets on every hop, but a public name that
//!     resolves to loopback goes unchecked behind a proxy. Disabling proxy
//!     support to close that would break every corporate user's fetch, which
//!     is a worse trade than the residual.
//!   * **Port granularity at the resolver.** `reqwest::dns::Name` carries no
//!     port, so an `allow` entry written as `dev.internal:8080` is honoured
//!     host-wide by [`EgressPolicy::check_resolved`] and port-exactly by
//!     `check_url`. The port fence is real — every request passes through
//!     `check_url` first — but the resolver half of it cannot be.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::Url;

/// Sentinel every refusal's `Display` starts with.
///
/// `reqwest::Error`'s own `Display` prints "error sending request for url (…)"
/// and never walks its source chain, so a refusal raised inside the resolver
/// or the redirect policy would otherwise surface as an opaque connect
/// failure. [`refusal_in_chain`] finds it again by this prefix.
pub const EGRESS_REFUSAL_PREFIX: &str = "stella egress guard: ";

/// A destination the policy refuses to reach.
///
/// Carries the whole operator-facing message, including the opt-out spelling
/// — the caller that surfaces it has no more context to add.
#[derive(Debug, Clone)]
pub struct Refusal(String);

impl Refusal {
    fn new(target: &str, class: &str) -> Self {
        Self(format!(
            "{EGRESS_REFUSAL_PREFIX}refusing to fetch {target} — {class} destinations are denied \
             by default, because a fetched page can steer the agent at one. To allow it, add the \
             host to `[egress] allow` in ~/.stella/web_auth.toml (e.g. \
             allow = [\"localhost:3000\"]); allow = [\"*\"] turns the guard off entirely."
        ))
    }

    /// The full operator-facing message.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// Recover a [`Refusal`] raised deep inside reqwest (resolver or redirect
/// policy) by walking the error's source chain for the sentinel prefix.
///
/// The chain is intact on both paths: hyper-util's `ConnectError` and its
/// legacy `Error` both implement `source()`, and reqwest stores the redirect
/// policy's error as the source of a `Kind::Redirect` error.
pub fn refusal_in_chain(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut cursor = Some(err);
    while let Some(current) = cursor {
        let rendered = current.to_string();
        if rendered.contains(EGRESS_REFUSAL_PREFIX) {
            return Some(rendered);
        }
        cursor = current.source();
    }
    None
}

/// One `[egress] allow` entry: a host, optionally pinned to one port.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AllowEntry {
    /// Lowercased, brackets stripped from an IPv6 literal.
    host: String,
    /// `None` means "this host on any port".
    port: Option<u16>,
}

/// The destination policy for one `web_auth.toml`.
///
/// `Default` is the shipping posture: deny the private ranges, allow the rest
/// of the internet.
#[derive(Debug, Default)]
pub struct EgressPolicy {
    /// `allow = ["*"]` — the wholesale opt-out. Kept as its own flag rather
    /// than a magic host so `cache_key` and `Debug` both read plainly.
    allow_all: bool,
    allow: Vec<AllowEntry>,
}

impl EgressPolicy {
    /// The shipping default: private destinations denied, no exemptions.
    pub fn deny_private() -> Self {
        Self::default()
    }

    /// The explicit wholesale opt-out (`allow = ["*"]`). Nothing else builds
    /// this — an operator has to write it down.
    pub fn allow_all() -> Self {
        Self {
            allow_all: true,
            allow: Vec::new(),
        }
    }

    /// Parse `["localhost:3000", "dev.internal", "[::1]:8080"]`.
    ///
    /// Entries are matched by EXACT host, not by registrable-domain suffix the
    /// way `[domains.…]` auth is: an exemption from a security default should
    /// name what it exempts, and `allow = ["example.com"]` must not quietly
    /// re-open every subdomain an attacker can register under it.
    pub fn from_allow_list<I, S>(entries: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Self::default();
        for raw in entries {
            let text = raw.as_ref().trim();
            if text == "*" {
                policy.allow_all = true;
                continue;
            }
            policy.allow.push(parse_allow_entry(text)?);
        }
        Ok(policy)
    }

    /// True when this policy exempts `host` — port-exactly if `port` is
    /// `Some`, host-wide if it is `None` (the resolver, which has no port).
    fn allows(&self, host: &str, port: Option<u16>) -> bool {
        if self.allow_all {
            return true;
        }
        let host = normalize_host(host);
        self.allow.iter().any(|entry| {
            entry.host == host && (entry.port.is_none() || port.is_none() || entry.port == port)
        })
    }

    /// Check a URL before it is requested, and again on every redirect hop.
    ///
    /// A URL with no host (which `fetch_raw` rejects for its own reasons) is
    /// not this function's problem, so it passes.
    pub fn check_url(&self, url: &Url) -> Result<(), Refusal> {
        let Some(host) = url.host_str() else {
            return Ok(());
        };
        if self.allows(host, url.port_or_known_default()) {
            return Ok(());
        }
        match classify_host(host) {
            Some(class) => Err(Refusal::new(&naming(url), class)),
            None => Ok(()),
        }
    }

    /// Check one address DNS returned for `name`.
    ///
    /// Port-blind on purpose — see the module header's second residual.
    pub fn check_resolved(&self, name: &str, ip: IpAddr) -> Result<(), Refusal> {
        if self.allows(name, None) {
            return Ok(());
        }
        match denied_class(ip) {
            Some(class) => Err(Refusal::new(&format!("`{name}` (resolved to {ip})"), class)),
            None => Ok(()),
        }
    }

    /// A canonical rendering, used to key the client pool so two configs with
    /// the same policy share one connection pool.
    pub fn cache_key(&self) -> String {
        if self.allow_all {
            return "*".to_string();
        }
        let sorted: BTreeSet<String> = self
            .allow
            .iter()
            .map(|entry| match entry.port {
                Some(port) => format!("{}:{port}", entry.host),
                None => entry.host.clone(),
            })
            .collect();
        sorted.into_iter().collect::<Vec<_>>().join(",")
    }
}

/// `host`, `host:port`, `[v6]`, or `[v6]:port`.
fn parse_allow_entry(text: &str) -> Result<AllowEntry, String> {
    let bad = |why: &str| format!("egress allow entry `{text}` {why}");
    let (host, port_text) = if let Some(rest) = text.strip_prefix('[') {
        // Bracketed IPv6 literal: the colons inside the brackets are the
        // address, not a port separator.
        let (inside, after) = rest
            .split_once(']')
            .ok_or_else(|| bad("opens a `[` that is never closed"))?;
        match after {
            "" => (inside, None),
            rest => (
                inside,
                Some(
                    rest.strip_prefix(':')
                        .ok_or_else(|| bad("has trailing text after `]`"))?,
                ),
            ),
        }
    } else {
        // A bare IPv6 literal has several colons and no port; anything with
        // exactly one colon is `host:port`.
        match text.split(':').count() {
            1 => (text, None),
            2 => {
                let (host, port) = text.split_once(':').expect("one colon");
                (host, Some(port))
            }
            _ => (text, None),
        }
    };
    if host.is_empty() {
        return Err(bad("has an empty host"));
    }
    let port = match port_text {
        Some(text) => Some(
            text.parse::<u16>()
                .map_err(|_| bad("has a port that is not a number in 0..=65535"))?,
        ),
        None => None,
    };
    Ok(AllowEntry {
        host: normalize_host(host),
        port,
    })
}

/// Lowercase, and strip the brackets `Url::host_str` keeps on an IPv6 literal
/// so `[::1]` and `::1` are the same key.
fn normalize_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

/// How a refused URL is named in the message: scheme, host, port and path,
/// and nothing else.
///
/// The whole URL is not used because a refusal is written into the transcript
/// the model reads, and on a redirect hop the URL is one the FETCHED PAGE
/// chose — `userinfo` and the query string are exactly where a credential
/// would sit. Scheme/host/port/path identifies what was refused without
/// carrying either into the model's context.
fn naming(url: &Url) -> String {
    let mut named = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
    if let Some(port) = url.port() {
        named.push(':');
        named.push_str(&port.to_string());
    }
    named.push_str(url.path());
    named
}

/// Classify the host component of a URL — an address if it parses as one,
/// otherwise a name.
///
/// `Url::host_str` hands back the WHATWG-normalised host, so the alternative
/// spellings of an address (`http://2130706433/`, `http://0177.0.0.1/`,
/// `http://127.1/`) have already been folded to their canonical dotted-quad by
/// the time they arrive; an IPv6 literal arrives bracketed.
fn classify_host(host: &str) -> Option<&'static str> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(v4) = bare.parse::<Ipv4Addr>() {
        return denied_v4(v4);
    }
    if let Ok(v6) = bare.parse::<Ipv6Addr>() {
        return denied_v6(v6);
    }
    denied_name(bare)
}

/// Name families that resolve inside the host's own network by construction.
///
/// These are refused by name as well as by address: `.internal` and `.local`
/// often resolve through mDNS or a split-horizon resolver that the address
/// table alone would not catch, and refusing them by name gives a far better
/// error than a connect failure.
fn denied_name(name: &str) -> Option<&'static str> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return Some("loopback (`localhost`)");
    }
    if name.ends_with(".internal") {
        return Some("internal-only (`.internal`, e.g. cloud instance metadata)");
    }
    if name.ends_with(".local") {
        return Some("link-local name (`.local`/mDNS)");
    }
    if name.ends_with(".home.arpa") {
        return Some("home-network (`.home.arpa`)");
    }
    None
}

/// The class `ip` falls in, or `None` when it is ordinary public space.
///
/// The stable `std` predicates are used where they exist; `is_shared` and
/// `is_benchmarking` are still behind the unstable `ip` feature, so those two
/// masks are written out.
fn denied_class(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => denied_v4(v4),
        IpAddr::V6(v6) => denied_v6(v6),
    }
}

fn denied_v4(v4: Ipv4Addr) -> Option<&'static str> {
    let octets = v4.octets();
    if v4.is_unspecified() {
        // 0.0.0.0 is "this host" to the connect(2) on every mainstream stack.
        return Some("unspecified (0.0.0.0)");
    }
    if v4.is_loopback() {
        return Some("loopback (127.0.0.0/8)");
    }
    if v4.is_private() {
        return Some("private (RFC1918)");
    }
    if v4.is_link_local() {
        return Some("link-local (169.254.0.0/16, cloud instance metadata)");
    }
    // 100.64.0.0/10 — carrier-grade NAT, routed inside many provider networks.
    if octets[0] == 100 && (octets[1] & 0b1100_0000) == 64 {
        return Some("carrier-grade NAT (100.64.0.0/10)");
    }
    // 198.18.0.0/15 — benchmarking.
    if octets[0] == 198 && (octets[1] & 0b1111_1110) == 18 {
        return Some("benchmarking (198.18.0.0/15)");
    }
    if v4.is_broadcast() {
        return Some("broadcast (255.255.255.255)");
    }
    if v4.is_multicast() {
        return Some("multicast (224.0.0.0/4)");
    }
    // 240.0.0.0/4 — reserved; `is_reserved` is unstable, and the range is
    // simply "the top sixteenth".
    if octets[0] >= 240 {
        return Some("reserved (240.0.0.0/4)");
    }
    None
}

fn denied_v6(v6: Ipv6Addr) -> Option<&'static str> {
    // An IPv4 address in a v6 costume still lands on the v4 destination:
    // `http://[::ffff:127.0.0.1]/` reaches loopback. Unwrap and re-run the v4
    // table BEFORE anything else, or it sails through every v6 predicate.
    if let Some(mapped) = v6.to_ipv4_mapped() {
        return denied_v4(mapped);
    }
    if v6.is_loopback() {
        return Some("loopback (::1)");
    }
    if v6.is_unspecified() {
        return Some("unspecified (::)");
    }
    // The deprecated IPv4-compatible form `::a.b.c.d`. Checked after loopback
    // and unspecified, which live in the same ::/96 and have better names.
    if let Some(compatible) = v6.to_ipv4() {
        return denied_v4(compatible);
    }
    let segments = v6.segments();
    // fc00::/7 — unique-local. `fd00:ec2::254` (the AWS IPv6 metadata
    // endpoint) is in here; the issue text called it link-local, which it is
    // not, so a fe80::/10 test alone would have missed it.
    if segments[0] & 0xfe00 == 0xfc00 {
        return Some("unique-local (fc00::/7, e.g. cloud instance metadata)");
    }
    // fe80::/10 — the real IPv6 link-local range.
    if segments[0] & 0xffc0 == 0xfe80 {
        return Some("link-local (fe80::/10)");
    }
    if v6.is_multicast() {
        return Some("multicast (ff00::/8)");
    }
    None
}

/// A `reqwest::dns::Resolve` that returns only the addresses the policy
/// approves.
///
/// Filtering rather than inspecting is deliberate: the set handed back is the
/// set hyper connects to, so there is no window in which a second resolution
/// could return a different answer (DNS rebinding). A name whose every address
/// is denied fails to resolve, carrying the [`Refusal`] as the error so
/// [`refusal_in_chain`] can recover the real message.
pub struct GuardedResolver {
    policy: Arc<EgressPolicy>,
}

impl GuardedResolver {
    pub fn new(policy: Arc<EgressPolicy>) -> Self {
        Self { policy }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_string();
            // Port 0: reqwest overwrites it with the URL's port (or the
            // scheme's default) after we return, exactly as the stock
            // resolver's callers do.
            let resolved = tokio::net::lookup_host((host.as_str(), 0u16))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let mut allowed: Vec<SocketAddr> = Vec::new();
            let mut refusal: Option<Refusal> = None;
            for addr in resolved {
                match policy.check_resolved(&host, addr.ip()) {
                    Ok(()) => allowed.push(addr),
                    Err(denied) => {
                        refusal.get_or_insert(denied);
                    }
                }
            }
            if allowed.is_empty() {
                let denied = refusal.unwrap_or_else(|| {
                    Refusal::new(&format!("`{host}`"), "unresolvable (no addresses)")
                });
                return Err(Box::new(denied) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use reqwest::dns::Resolve;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test address")
    }

    #[test]
    fn the_denied_classes_cover_every_range_that_reaches_inside_the_host() {
        for (address, expected) in [
            ("127.0.0.1", "loopback"),
            ("127.9.9.9", "loopback"),
            ("169.254.169.254", "link-local"),
            ("10.0.0.1", "private"),
            ("172.16.0.1", "private"),
            ("192.168.1.1", "private"),
            ("100.64.0.1", "carrier-grade NAT"),
            ("198.18.0.1", "benchmarking"),
            ("0.0.0.0", "unspecified"),
            ("255.255.255.255", "broadcast"),
            ("224.0.0.1", "multicast"),
            ("240.0.0.1", "reserved"),
            ("::1", "loopback"),
            ("::", "unspecified"),
            ("fd00:ec2::254", "unique-local"),
            ("fc00::1", "unique-local"),
            ("fe80::1", "link-local"),
            ("ff02::1", "multicast"),
            // The costume cases: both must be unwrapped and re-tested.
            ("::ffff:127.0.0.1", "loopback"),
            ("::ffff:169.254.169.254", "link-local"),
            ("::ffff:10.0.0.1", "private"),
            ("::127.0.0.1", "loopback"),
        ] {
            let class = denied_class(ip(address))
                .unwrap_or_else(|| panic!("{address} must be denied, was allowed"));
            assert!(
                class.contains(expected),
                "{address} denied as `{class}`, expected a `{expected}` class"
            );
        }
    }

    #[test]
    fn ordinary_public_addresses_are_allowed() {
        for address in [
            "93.184.216.34",
            "1.1.1.1",
            "8.8.8.8",
            "2606:2800:220:1::1",
            "2001:4860:4860::8888",
            "::ffff:93.184.216.34",
        ] {
            assert_eq!(
                denied_class(ip(address)),
                None,
                "{address} must be reachable"
            );
        }
    }

    #[test]
    fn the_default_policy_refuses_loopback_and_metadata_urls() {
        let policy = EgressPolicy::deny_private();
        for url in [
            "http://127.0.0.1:3000/",
            "http://localhost:3000/",
            "http://dev.localhost/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fd00:ec2::254]/latest/meta-data/",
            "http://[::ffff:127.0.0.1]:8080/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://printer.local/",
            "http://10.1.2.3/",
            // Alternative spellings of 127.0.0.1 the URL parser normalises
            // before we ever see them. Asserted so a parser change that stops
            // normalising cannot open a hole silently.
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://127.1/",
        ] {
            let parsed = Url::parse(url).expect("test url");
            let refusal = policy
                .check_url(&parsed)
                .expect_err(&format!("{url} must be refused"));
            assert!(
                refusal.message().starts_with(EGRESS_REFUSAL_PREFIX),
                "{}",
                refusal.message()
            );
            assert!(
                refusal.message().contains("[egress] allow"),
                "a refusal must name its own opt-out: {}",
                refusal.message()
            );
        }
        assert!(
            policy
                .check_url(&Url::parse("https://example.com/x").unwrap())
                .is_ok()
        );
    }

    /// A refusal is written into the transcript the model reads, and on a
    /// redirect hop the URL is one the fetched PAGE chose — so `userinfo` and
    /// the query string must not ride along into the model's context.
    #[test]
    fn a_refusal_names_the_destination_without_its_credentials_or_query() {
        let url = Url::parse("http://user:hunter2@127.0.0.1:3000/admin?token=secret-value#frag")
            .expect("test url");
        let message = EgressPolicy::deny_private()
            .check_url(&url)
            .expect_err("loopback is refused")
            .message()
            .to_string();
        assert!(message.contains("http://127.0.0.1:3000/admin"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
        assert!(!message.contains("secret-value"), "{message}");
    }

    #[test]
    fn an_allow_entry_can_pin_a_port_and_a_bare_host_covers_all_of_them() {
        let policy = EgressPolicy::from_allow_list(["127.0.0.1:3000", "dev.internal"]).unwrap();
        assert!(
            policy
                .check_url(&Url::parse("http://127.0.0.1:3000/app").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .check_url(&Url::parse("http://127.0.0.1:9999/app").unwrap())
                .is_err()
        );
        assert!(
            policy
                .check_url(&Url::parse("http://dev.internal/x").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .check_url(&Url::parse("http://dev.internal:8443/x").unwrap())
                .is_ok()
        );
        // Exact host, not registrable-domain suffix: an exemption names what
        // it exempts.
        assert!(
            policy
                .check_url(&Url::parse("http://evil.dev.internal/x").unwrap())
                .is_err()
        );
    }

    #[test]
    fn a_bracketed_ipv6_allow_entry_parses_its_port() {
        let policy = EgressPolicy::from_allow_list(["[::1]:8080"]).unwrap();
        assert!(
            policy
                .check_url(&Url::parse("http://[::1]:8080/x").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .check_url(&Url::parse("http://[::1]:8081/x").unwrap())
                .is_err()
        );
        let any_port = EgressPolicy::from_allow_list(["::1"]).unwrap();
        assert!(
            any_port
                .check_url(&Url::parse("http://[::1]:8081/x").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn the_wholesale_opt_out_allows_everything_and_keys_distinctly() {
        let policy = EgressPolicy::from_allow_list(["*"]).unwrap();
        assert!(
            policy
                .check_url(&Url::parse("http://169.254.169.254/").unwrap())
                .is_ok()
        );
        assert_eq!(policy.cache_key(), "*");
        assert_eq!(EgressPolicy::allow_all().cache_key(), "*");
        assert_eq!(EgressPolicy::deny_private().cache_key(), "");
        assert_ne!(
            EgressPolicy::from_allow_list(["127.0.0.1"])
                .unwrap()
                .cache_key(),
            EgressPolicy::deny_private().cache_key()
        );
        // Order and case must not fork the client pool.
        assert_eq!(
            EgressPolicy::from_allow_list(["b.example:1", "A.example"])
                .unwrap()
                .cache_key(),
            EgressPolicy::from_allow_list(["a.example", "b.example:1"])
                .unwrap()
                .cache_key()
        );
    }

    #[test]
    fn a_malformed_allow_entry_is_an_error_not_a_silent_no_op() {
        assert!(EgressPolicy::from_allow_list(["host:not-a-port"]).is_err());
        assert!(EgressPolicy::from_allow_list(["[::1"]).is_err());
        assert!(EgressPolicy::from_allow_list([":8080"]).is_err());
    }

    /// The post-DNS half of the guard: a NAME is not what gets checked, the
    /// addresses it resolves to are. `localhost` is the one name every host
    /// resolves offline, and it resolves to exactly what the policy denies.
    #[tokio::test]
    async fn the_resolver_refuses_a_name_whose_addresses_are_all_denied() {
        let resolver = GuardedResolver::new(Arc::new(EgressPolicy::deny_private()));
        let error = resolver
            .resolve(reqwest::dns::Name::from_str("localhost").unwrap())
            .await
            .err()
            .expect("`localhost` resolves to loopback, which is denied");
        let rendered = error.to_string();
        assert!(rendered.contains(EGRESS_REFUSAL_PREFIX), "{rendered}");
        assert!(rendered.contains("loopback"), "{rendered}");
        assert!(
            refusal_in_chain(error.as_ref()).is_some(),
            "the refusal must be recoverable from the error chain"
        );
    }

    #[tokio::test]
    async fn the_resolver_lets_an_allow_listed_name_through() {
        let resolver = GuardedResolver::new(Arc::new(
            EgressPolicy::from_allow_list(["localhost"]).unwrap(),
        ));
        let addrs = resolver
            .resolve(reqwest::dns::Name::from_str("localhost").unwrap())
            .await
            .expect("an allow-listed name resolves");
        assert!(
            addrs.count() > 0,
            "the allow-listed name must keep its addresses"
        );
    }
}
