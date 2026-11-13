//! Background feed polling loop, lease-based outbox delivery worker (pacing + jitter + quotas),
//! and periodic payload GC / `PRAGMA optimize` maintenance loop.
//!
//! Implemented in Wave 2 (`feedcourier.md` Section 三.3 & 四.3).

use futures_util as _;
use tokio_util as _;

/// Default lease duration in seconds granted when a worker claims a `delivery_tasks` row.
pub const DEFAULT_TASK_LEASE_SECS: i64 = 120;

/// Default maximum delivery attempts before moving a task to `dead_letter`.
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: i64 = 6;
