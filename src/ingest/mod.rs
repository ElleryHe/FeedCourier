//! Feed ingestion pipeline: `feed-rs` parsing, 4-tier anti-drift SHA-256 fingerprinting,
//! relative URL resolution, `dom_smoothie` Readability extraction, `ammonia` HTML sanitization,
//! Unicode first-sentence segmentation, and OPML import/export.
//!
//! Implemented in Wave 2 (`feedcourier.md` Section 四.1 & 四.2).

use ammonia as _;
use dom_smoothie as _;
use feed_rs as _;
use quick_xml as _;
use regex as _;
use unicode_segmentation as _;

/// Default concurrency limit for on-demand Readability full-text extraction (`feedcourier.md` Section 四.1).
pub const DEFAULT_READABILITY_CONCURRENCY: usize = 2;

/// Consecutive poll failure threshold before tripping the feed circuit breaker (`circuit_open`).
pub const DEFAULT_CIRCUIT_BREAKER_THRESHOLD: i64 = 15;
