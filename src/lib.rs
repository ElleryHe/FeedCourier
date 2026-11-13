//! FeedCourier core library.

pub mod api;
pub mod channels;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod ingest;
pub mod mcp;
pub mod net;
pub mod scheduler;

pub use crate::api::{bootstrap_env_settings, build_full_router};
