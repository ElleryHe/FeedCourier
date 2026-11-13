//! Connect-time DNS & IP validation (`SsrfPolicy` + `SsrfDnsResolver`) defending against
//! SSRF, cloud metadata exfiltration (`169.254.169.254`), and DNS rebinding (TOCTOU).

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, OnceLock, RwLock};

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{GOOGLE, LookupIpStrategy, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

use crate::error::{AppError, AppResult};

/// Cloud metadata IPv4 endpoint (`169.254.169.254`) used by AWS, GCP, Azure, OpenStack, etc.
pub const CLOUD_METADATA_IPV4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);

/// AWS IMDSv2 IPv6 endpoint (`fd00:ec2::254`).
pub const CLOUD_METADATA_IPV6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Marker error returned inside `reqwest::dns::Resolve` so outer `ProtectedHttpClient`
/// accurately surfaces `AppError::SsrfBlocked` rather than generic transport failure.
#[derive(Debug, Clone)]
pub struct SsrfBlockedError(pub String);

impl fmt::Display for SsrfBlockedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SSRF guard blocked target: {}", self.0)
    }
}

impl StdError for SsrfBlockedError {}

/// Walk a `std::error::Error` source chain to extract an embedded [`SsrfBlockedError`], if any.
pub fn find_ssrf_blocked_in_error(err: &(dyn StdError + 'static)) -> Option<String> {
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(ssrf) = e.downcast_ref::<SsrfBlockedError>() {
            return Some(ssrf.0.clone());
        }
        current = e.source();
    }
    None
}

/// Custom async DNS lookup hook used for deterministic unit/integration testing of DNS rebinding.
pub type CustomDnsLookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, String>> + Send>>
        + Send
        + Sync,
>;

/// Parsed, immutable SSRF policy compiled from `NetSettings::ssrf_allowlist`.
#[derive(Debug, Clone, Default)]
pub struct SsrfPolicy {
    /// Allowed `ipnet::IpNet` CIDR blocks (e.g. `10.0.0.0/8`, `127.0.0.1/32`).
    cidrs: Vec<IpNet>,
    /// Allowed exact lowercase hostnames without trailing dot (e.g. `rsshub.internal`, `127.0.0.1`).
    exact_hosts: Vec<String>,
    /// Allowed wildcard hostname suffixes stored WITH leading dot (e.g. `.lan` from `*.lan`).
    wildcard_suffixes: Vec<String>,
    /// True ONLY when `169.254.169.254/32` was explicitly listed in `ssrf_allowlist`.
    allow_cloud_metadata_v4: bool,
    /// True ONLY when `fd00:ec2::254/128` was explicitly listed in `ssrf_allowlist`.
    allow_cloud_metadata_v6: bool,
}

impl SsrfPolicy {
    /// Parse and validate `ssrf_allowlist` entries (`CIDR`, bare IP, `*.domain` wildcard, or exact hostname).
    pub fn from_allowlist(entries: &[String]) -> AppResult<Self> {
        let mut policy = Self::default();

        for raw in entries {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.eq_ignore_ascii_case("169.254.169.254/32") {
                policy.allow_cloud_metadata_v4 = true;
            }
            if trimmed.eq_ignore_ascii_case("fd00:ec2::254/128") {
                policy.allow_cloud_metadata_v6 = true;
            }

            // 1. Check explicit CIDR notation (`ip/prefix`).
            if trimmed.contains('/') {
                let net = IpNet::from_str(trimmed).map_err(|err| {
                    AppError::Validation(format!(
                        "invalid CIDR entry '{trimmed}' in ssrf_allowlist: {err}"
                    ))
                })?;
                policy.cidrs.push(net.trunc());
                continue;
            }

            // 2. Check bare IP literal (strip optional IPv6 brackets `[::1]`).
            let unbracketed = trimmed
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(trimmed);
            if let Ok(ip) = IpAddr::from_str(unbracketed) {
                let canon = canonicalize_ip(ip);
                // Note: bare `169.254.169.254` intentionally does NOT set `allow_cloud_metadata_v4`;
                // `169.254.169.254/32` is required to unlock cloud metadata.
                policy.cidrs.push(IpNet::from(canon));
                policy
                    .exact_hosts
                    .push(canon.to_string().to_ascii_lowercase());
                continue;
            }

            // 3. Check wildcard hostname (`*.lan`).
            let normalized = normalize_hostname(trimmed);
            if normalized.is_empty() {
                continue;
            }
            if let Some(rest) = normalized.strip_prefix("*.") {
                if rest.is_empty() || rest.contains('*') || rest.contains('/') {
                    return Err(AppError::Validation(format!(
                        "invalid wildcard hostname '{trimmed}' in ssrf_allowlist"
                    )));
                }
                policy.wildcard_suffixes.push(format!(".{rest}"));
                continue;
            }

            if normalized.contains('*') || normalized.contains(' ') {
                return Err(AppError::Validation(format!(
                    "invalid hostname entry '{trimmed}' in ssrf_allowlist"
                )));
            }

            policy.exact_hosts.push(normalized);
        }

        Ok(policy)
    }

    /// Returns `true` if `hostname` matches an exact or wildcard hostname entry in `ssrf_allowlist`.
    pub fn is_hostname_allowlisted(&self, hostname: &str) -> bool {
        let norm = normalize_hostname(hostname);
        if norm.is_empty() {
            return false;
        }
        if self.exact_hosts.iter().any(|h| h == &norm) {
            return true;
        }
        self.wildcard_suffixes
            .iter()
            .any(|suffix| norm.len() > suffix.len() && norm.ends_with(suffix.as_str()))
    }

    /// Validate a single hop URL's scheme and host (including literal IPs, `localhost` under
    /// remote-DNS proxies, etc.) BEFORE opening a socket.
    pub fn validate_url_hop(&self, url: &Url, remote_proxy_dns: bool) -> AppResult<()> {
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(AppError::SsrfBlocked(format!(
                    "disallowed URL scheme '{other}' in '{url}' (only http and https are permitted)"
                )));
            }
        }

        let Some(host) = url.host() else {
            return Err(AppError::Validation(format!(
                "URL '{url}' is missing a valid host"
            )));
        };

        match host {
            Host::Ipv4(v4) => self.check_resolved_ips(&v4.to_string(), &[IpAddr::V4(v4)]),
            Host::Ipv6(v6) => self.check_resolved_ips(&v6.to_string(), &[IpAddr::V6(v6)]),
            Host::Domain(domain) => {
                let norm = normalize_hostname(domain);
                if norm.is_empty() {
                    return Err(AppError::Validation(format!(
                        "URL '{url}' has an empty domain"
                    )));
                }
                // Defense-in-depth when using `socks5h` / HTTP proxy (where the proxy resolves DNS):
                // still reject obvious local/metadata hostnames unless allowlisted.
                if remote_proxy_dns
                    && (norm == "localhost"
                        || norm.ends_with(".localhost")
                        || norm == "metadata.google.internal")
                    && !self.is_hostname_allowlisted(&norm)
                {
                    // Permit `localhost` only if 127.0.0.1 AND ::1 are CIDR-allowlisted, otherwise block.
                    let loopback_allowed = self
                        .is_ip_cidr_allowlisted(IpAddr::V4(Ipv4Addr::LOCALHOST))
                        && self.is_ip_cidr_allowlisted(IpAddr::V6(Ipv6Addr::LOCALHOST));
                    if !loopback_allowed || norm == "metadata.google.internal" {
                        return Err(AppError::SsrfBlocked(format!(
                            "internal hostname '{domain}' is blocked by SSRF guard"
                        )));
                    }
                }
                Ok(())
            }
        }
    }

    /// Validate **every** IP resolved for `hostname` at connect time.
    ///
    /// Rules (`feedcourier.md` Section 四.1):
    /// 1. `169.254.169.254` (and `::ffff:169.254.169.254`) is **NEVER** allowed unless
    ///    `169.254.169.254/32` is explicitly present in `ssrf_allowlist` (even if the hostname or
    ///    a wider CIDR like `169.254.0.0/16` was allowlisted!).
    /// 2. Private/loopback/internal IPs are allowed ONLY if `hostname` matches an exact/wildcard
    ///    hostname in `ssrf_allowlist`, OR **all** resolved IPs match at least one CIDR in
    ///    `ssrf_allowlist`.
    pub fn check_resolved_ips(&self, hostname: &str, ips: &[IpAddr]) -> AppResult<()> {
        if ips.is_empty() {
            return Err(AppError::SsrfBlocked(format!(
                "hostname '{hostname}' resolved to zero IP addresses"
            )));
        }

        // Step 1: Unconditional cloud metadata guard unless exact `/32` (or `/128`) was listed.
        for &raw_ip in ips {
            let ip = canonicalize_ip(raw_ip);
            if ip == IpAddr::V4(CLOUD_METADATA_IPV4) && !self.allow_cloud_metadata_v4 {
                return Err(AppError::SsrfBlocked(format!(
                    "target '{hostname}' resolved to cloud metadata address 169.254.169.254"
                )));
            }
            if ip == IpAddr::V6(CLOUD_METADATA_IPV6) && !self.allow_cloud_metadata_v6 {
                return Err(AppError::SsrfBlocked(format!(
                    "target '{hostname}' resolved to cloud metadata address fd00:ec2::254"
                )));
            }
        }

        // Step 2: Hostname exact / wildcard allowlist override.
        if self.is_hostname_allowlisted(hostname) {
            return Ok(());
        }

        // Step 3: Every non-public IP must be covered by an explicit CIDR in `ssrf_allowlist`.
        for &raw_ip in ips {
            let ip = canonicalize_ip(raw_ip);
            if let Some(reason) = classify_forbidden_ip(ip)
                && !self.is_ip_cidr_allowlisted(ip)
            {
                return Err(AppError::SsrfBlocked(format!(
                    "target '{hostname}' resolved to {reason} ({raw_ip})"
                )));
            }
        }

        Ok(())
    }

    fn is_ip_cidr_allowlisted(&self, ip: IpAddr) -> bool {
        let canon = canonicalize_ip(ip);
        self.cidrs.iter().any(|net| net.contains(&canon))
    }
}

/// Normalize a hostname by stripping optional `[...]` brackets, trailing DNS root dot, and lowercasing.
fn normalize_hostname(host: &str) -> String {
    let trimmed = host.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    unbracketed.trim_end_matches('.').to_ascii_lowercase()
}

/// Canonicalize IPv4-mapped IPv6 (`::ffff:a.b.c.d`) and NAT64 well-known prefix (`64:ff9b::/96`)
/// into their underlying `IpAddr::V4` so IPv6-mapped bypass tricks cannot evade IPv4 checks.
pub fn canonicalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let segs = v6.segments();
            // RFC 6052 Well-Known NAT64 Prefix `64:ff9b::/96` and RFC 8215 Local-Use `64:ff9b:1::/48`.
            if (segs[..6] == [0x0064, 0xff9b, 0, 0, 0, 0])
                || (segs[..3] == [0x0064, 0xff9b, 0x0001])
            {
                let hi = segs[6].to_be_bytes();
                let lo = segs[7].to_be_bytes();
                return IpAddr::V4(Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]));
            }
            IpAddr::V6(v6)
        }
    }
}

/// Classify whether `ip` belongs to any non-global / private / loopback / metadata / reserved range.
/// Returns `Some(human_reason)` if forbidden by default, or `None` if globally routable unicast.
pub fn classify_forbidden_ip(ip: IpAddr) -> Option<&'static str> {
    match canonicalize_ip(ip) {
        IpAddr::V4(v4) => classify_forbidden_ipv4(v4),
        IpAddr::V6(v6) => classify_forbidden_ipv6(v6),
    }
}

fn classify_forbidden_ipv4(v4: Ipv4Addr) -> Option<&'static str> {
    if v4 == CLOUD_METADATA_IPV4 {
        return Some("cloud metadata address (169.254.169.254)");
    }
    let octets = v4.octets();

    // 0.0.0.0/8 ("This network" / unspecified, RFC 1122)
    if octets[0] == 0 {
        return Some("unspecified network (0.0.0.0/8)");
    }
    // 127.0.0.0/8 (Loopback, RFC 1122)
    if v4.is_loopback() {
        return Some("loopback address (127.0.0.0/8)");
    }
    // 169.254.0.0/16 (Link-local, RFC 3927)
    if v4.is_link_local() {
        return Some("link-local address (169.254.0.0/16)");
    }
    // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 (RFC 1918 private)
    if v4.is_private() {
        return Some("RFC1918 private network");
    }
    // 100.64.0.0/10 (Carrier-Grade NAT / Shared Address Space, RFC 6598)
    if octets[0] == 100 && (octets[1] & 0b1100_0000) == 64 {
        return Some("CGNAT shared address space (100.64.0.0/10)");
    }
    // 192.0.0.0/24 (IETF Protocol Assignments, RFC 6890)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return Some("IETF protocol assignment range (192.0.0.0/24)");
    }
    // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 (Documentation, RFC 5737)
    if v4.is_documentation() {
        return Some("documentation network (TEST-NET)");
    }
    // 192.88.99.0/24 (Deprecated 6to4 relay anycast, RFC 7526)
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return Some("deprecated 6to4 relay anycast (192.88.99.0/24)");
    }
    // 198.18.0.0/15 (Benchmarking, RFC 2544)
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return Some("benchmarking network (198.18.0.0/15)");
    }
    // 224.0.0.0/4 (Multicast, RFC 5771)
    if v4.is_multicast() {
        return Some("multicast address (224.0.0.0/4)");
    }
    // 240.0.0.0/4 (Reserved for future use + 255.255.255.255 broadcast)
    if v4.is_broadcast() || (octets[0] & 0xf0) == 240 {
        return Some("reserved or broadcast address (240.0.0.0/4)");
    }

    None
}

fn classify_forbidden_ipv6(v6: Ipv6Addr) -> Option<&'static str> {
    if v6 == CLOUD_METADATA_IPV6 {
        return Some("cloud metadata address (fd00:ec2::254)");
    }
    if v6.is_unspecified() {
        return Some("IPv6 unspecified address (::)");
    }
    if v6.is_loopback() {
        return Some("IPv6 loopback address (::1)");
    }
    let segs = v6.segments();

    // Deprecated IPv4-compatible IPv6 `::a.b.c.d`
    if segs[..6] == [0, 0, 0, 0, 0, 0] {
        return Some("deprecated IPv4-compatible IPv6 address");
    }
    // Unique Local Address `fc00::/7` (RFC 4193)
    if (segs[0] & 0xfe00) == 0xfc00 {
        return Some("IPv6 unique local address (fc00::/7)");
    }
    // Link-Local Unicast `fe80::/10` (RFC 4291)
    if (segs[0] & 0xffc0) == 0xfe80 {
        return Some("IPv6 link-local address (fe80::/10)");
    }
    // Deprecated Site-Local `fec0::/10`
    if (segs[0] & 0xffc0) == 0xfec0 {
        return Some("deprecated IPv6 site-local address (fec0::/10)");
    }
    // Multicast `ff00::/8`
    if v6.is_multicast() {
        return Some("IPv6 multicast address (ff00::/8)");
    }
    // Discard-Only `100::/64` (RFC 6666)
    if segs[..4] == [0x0100, 0, 0, 0] {
        return Some("IPv6 discard-only range (100::/64)");
    }
    // Documentation `2001:db8::/32` (RFC 3849)
    if segs[0] == 0x2001 && segs[1] == 0x0db8 {
        return Some("IPv6 documentation range (2001:db8::/32)");
    }
    // Benchmarking `2001:2::/48` (RFC 5180)
    if segs[0] == 0x2001 && segs[1] == 0x0002 && segs[2] == 0 {
        return Some("IPv6 benchmarking range (2001:2::/48)");
    }
    // Teredo `2001::/32` (RFC 4380) — embeds arbitrary obfuscated IPv4 targets
    if segs[0] == 0x2001 && segs[1] == 0x0000 {
        return Some("IPv6 Teredo tunneling range (2001::/32)");
    }
    // 6to4 `2002::/16` (RFC 3056) — embeds IPv4 in segments[1..3]
    if segs[0] == 0x2002 {
        let hi = segs[1].to_be_bytes();
        let lo = segs[2].to_be_bytes();
        let embedded = Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]);
        if classify_forbidden_ipv4(embedded).is_some() {
            return Some("6to4 address embedding non-global IPv4");
        }
    }

    None
}

/// Connect-time `reqwest::dns::Resolve` implementation backed by `hickory-resolver` that validates
/// every resolved `IpAddr` against the live [`SsrfPolicy`] before returning socket addresses.
#[derive(Clone)]
pub struct SsrfDnsResolver {
    policy: Arc<RwLock<SsrfPolicy>>,
    hickory: Arc<OnceLock<TokioResolver>>,
    custom_lookup: Option<CustomDnsLookup>,
}

impl fmt::Debug for SsrfDnsResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SsrfDnsResolver")
            .field("has_custom_lookup", &self.custom_lookup.is_some())
            .finish()
    }
}

impl SsrfDnsResolver {
    pub fn new(policy: Arc<RwLock<SsrfPolicy>>) -> Self {
        Self {
            policy,
            hickory: Arc::new(OnceLock::new()),
            custom_lookup: None,
        }
    }

    pub fn with_custom_lookup(policy: Arc<RwLock<SsrfPolicy>>, lookup: CustomDnsLookup) -> Self {
        Self {
            policy,
            hickory: Arc::new(OnceLock::new()),
            custom_lookup: Some(lookup),
        }
    }

    /// Resolve `host` and validate all returned IPs against the current `SsrfPolicy`.
    pub async fn resolve_and_validate(&self, host: &str) -> AppResult<Vec<IpAddr>> {
        let clean_host = host
            .trim()
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host.trim());

        let ips = if let Ok(literal_ip) = IpAddr::from_str(clean_host) {
            vec![literal_ip]
        } else if let Some(custom) = &self.custom_lookup {
            (custom)(clean_host.to_string()).await.map_err(|err| {
                AppError::Upstream(format!("DNS lookup failed for '{clean_host}': {err}"))
            })?
        } else {
            let resolver = self.get_or_init_hickory()?;
            let lookup = resolver.lookup_ip(clean_host).await.map_err(|err| {
                AppError::Upstream(format!("DNS lookup failed for '{clean_host}': {err}"))
            })?;
            lookup.iter().collect::<Vec<IpAddr>>()
        };

        let policy = self
            .policy
            .read()
            .expect("SSRF policy RwLock poisoned")
            .clone();
        policy.check_resolved_ips(clean_host, &ips)?;
        Ok(ips)
    }

    fn get_or_init_hickory(&self) -> AppResult<&TokioResolver> {
        if let Some(existing) = self.hickory.get() {
            return Ok(existing);
        }
        let mut builder = TokioResolver::builder_tokio().unwrap_or_else(|_| {
            TokioResolver::builder_with_config(
                ResolverConfig::udp_and_tcp(&GOOGLE),
                TokioRuntimeProvider::default(),
            )
        });
        builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        let built = builder
            .build()
            .map_err(|err| AppError::Config(format!("failed to initialize DNS resolver: {err}")))?;
        let _ = self.hickory.set(built);
        Ok(self.hickory.get().expect("TokioResolver initialized"))
    }
}

impl Resolve for SsrfDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            match this.resolve_and_validate(&host).await {
                Ok(ips) => {
                    let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
                    Ok(addrs)
                }
                Err(AppError::SsrfBlocked(msg)) => {
                    // Strip any duplicate prefix since AppError::SsrfBlocked already formats nicely.
                    let detail = msg
                        .strip_prefix("SSRF guard blocked target: ")
                        .unwrap_or(&msg)
                        .to_string();
                    Err(Box::new(SsrfBlockedError(detail)) as Box<dyn StdError + Send + Sync>)
                }
                Err(other) => Err(Box::new(std::io::Error::other(other.to_string()))
                    as Box<dyn StdError + Send + Sync>),
            }
        })
    }
}
