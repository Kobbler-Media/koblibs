//! Shared types and HTTP client for koblem — the local LLM service.
//!
//! Types in this crate are the wire format between koblem and its callers.
//! Both koblem (the server) and kobbler/kobload (the clients) depend on this crate
//! to keep request/response types in sync.

pub mod embed;
pub mod types;

mod client;
pub use client::{Error, KoblemClient};
