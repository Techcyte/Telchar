//! Exposes typed gateway-store access, transfer, validation, promotion, and retention.

pub mod closure;
pub mod daemon;
pub mod export;
pub mod import;
pub mod nar;
pub mod promotion;
pub mod query;
pub mod retention;
pub mod runtime;
pub mod substitution;

pub use daemon::{GatewayStoreConnection, GatewayStoreEndpoint};
