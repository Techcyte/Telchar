//! Tests store-lease, retained-byte, and request-release persistence contracts and failure boundaries.

mod support;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use support::postgres::PostgresFixture;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

static TELEMETRY_TESTS: Mutex<()> = Mutex::new(());

#[path = "persistence_leases/input.rs"]
mod input;
#[path = "persistence_leases/output.rs"]
mod output;
#[path = "persistence_leases/release.rs"]
mod release;
#[path = "persistence_leases/store.rs"]
mod store;

#[derive(Clone, Default)]
struct EventCapture(Arc<Mutex<Vec<String>>>);

impl EventCapture {
    fn events(&self) -> Vec<String> {
        self.0.lock().expect("events lock").clone()
    }
}

impl<S> Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        self.0.lock().expect("events lock").push(fields.0);
    }
}

#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&format!("{value:?}"));
    }
}

fn purpose_name(purpose: telchar::persistence::StoreLeasePurpose) -> &'static str {
    match purpose {
        telchar::persistence::StoreLeasePurpose::Input => "session-input",
        telchar::persistence::StoreLeasePurpose::Transfer => "session-transfer",
        _ => unreachable!("tested purpose has a stable fixture name"),
    }
}
