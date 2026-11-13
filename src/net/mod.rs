//! Protected outbound HTTP client, connect-time DNS/SSRF guard, hop-by-hop redirect validation,
//! response body size caps, and optional HTTP/SOCKS5 proxy routing.
//!
//! Implemented in Wave 2 (`feedcourier.md` Section 四.1).

use hickory_resolver as _;
use ipnet as _;
use reqwest as _;

/// Default maximum response body size when fetching RSS/Atom/JSON Feeds (`10 MiB`).
pub const DEFAULT_MAX_FEED_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum response body size when fetching article HTML for Readability (`5 MiB`).
pub const DEFAULT_MAX_ARTICLE_BYTES: usize = 5 * 1024 * 1024;

/// Default maximum HTTP redirect hops validated individually against the SSRF guard.
pub const DEFAULT_MAX_REDIRECT_HOPS: usize = 5;
