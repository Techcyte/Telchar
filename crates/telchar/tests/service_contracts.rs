//! Service API, IPC buffering, and transfer admission contracts.

#[path = "service_contracts/ipc_buffer.rs"]
mod ipc_buffer;
#[path = "service_contracts/public_api.rs"]
mod public_api;
#[path = "service_contracts/transfer_limits.rs"]
mod transfer_limits;
