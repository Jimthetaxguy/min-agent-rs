//! Read-only agent core. Provider transport and filesystem execution are separate.
#![forbid(unsafe_code)]
pub mod adapters;
pub mod agent;
pub mod config;
pub mod model;
pub mod redact;
pub mod tools;
pub mod trace;
