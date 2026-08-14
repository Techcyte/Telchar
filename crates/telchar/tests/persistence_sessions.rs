//! Tests protocol-session, request, and attachment persistence contracts and failure boundaries.

mod support;

use std::sync::Arc;
use std::thread;

use support::postgres::PostgresFixture;

#[path = "persistence_sessions/attachments.rs"]
mod attachments;
#[path = "persistence_sessions/build_requests.rs"]
mod build_requests;
#[path = "persistence_sessions/executions.rs"]
mod executions;
#[path = "persistence_sessions/protocol_sessions.rs"]
mod protocol_sessions;
