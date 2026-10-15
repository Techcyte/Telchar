//! Tests protocol-session, request, and attachment persistence contracts and failure boundaries.

use telchar_test_support::postgres::PostgresFixture;

use std::sync::Arc;
use std::thread;

#[path = "persistence_sessions/attachments.rs"]
mod attachments;
#[path = "persistence_sessions/build_requests.rs"]
mod build_requests;
#[path = "persistence_sessions/executions.rs"]
mod executions;
#[path = "persistence_sessions/protocol_sessions.rs"]
mod protocol_sessions;
