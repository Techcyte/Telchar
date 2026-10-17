//! Tests store-lease, retained-byte, and request-release persistence contracts and failure boundaries.

struct PostgresFixture {
    database: telchar::persistence::Database,
    server: telchar_test_support::postgres::PostgresFixture,
}

impl PostgresFixture {
    fn start() -> Self {
        let server = telchar_test_support::postgres::PostgresFixture::start();
        let database = telchar::persistence::Database::connect(server.url()).unwrap();
        Self { database, server }
    }

    fn database(&self) -> &telchar::persistence::Database {
        &self.database
    }
}

impl std::ops::Deref for PostgresFixture {
    type Target = telchar_test_support::postgres::PostgresFixture;

    fn deref(&self) -> &Self::Target {
        &self.server
    }
}

impl std::ops::DerefMut for PostgresFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.server
    }
}

use std::fmt;
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

static TELEMETRY_TESTS: Mutex<()> = Mutex::new(());

#[path = "persistence_leases/batch.rs"]
mod batch;
#[path = "persistence_leases/input.rs"]
mod input;
#[path = "persistence_leases/output.rs"]
mod output;
#[path = "persistence_leases/release.rs"]
mod release;
#[path = "persistence_leases/store.rs"]
mod store;

#[derive(Clone)]
struct EventCapture(Arc<Mutex<Vec<String>>>);

impl Default for EventCapture {
    fn default() -> Self {
        static SUBSCRIBER: Once = Once::new();
        SUBSCRIBER.call_once(|| {
            // Shared callsites can first execute on threads outside the capture scope.
            tracing::subscriber::set_global_default(tracing_subscriber::registry())
                .expect("global telemetry subscriber installs");
        });
        Self(Arc::default())
    }
}

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
