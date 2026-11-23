//! Tests server-side work lifetime after a request deadline.

use super::*;

#[test]
fn request_deadline_stops_server_work() {
    deadline_stops_server_work(false);
}

#[test]
fn tls_request_deadline_stops_server_work() {
    deadline_stops_server_work(true);
}

fn deadline_stops_server_work(tls: bool) {
    let server = if tls {
        telchar_test_support::postgres::PostgresFixture::start_tls()
    } else {
        telchar_test_support::postgres::PostgresFixture::start()
    };
    let fixture = PostgresFixture {
        database: telchar::persistence::Database::connect(server.url()).unwrap(),
        server,
    };
    telchar::persistence::migrate(fixture.database()).unwrap();
    fixture.connect().batch_execute(
        "INSERT INTO build_requests(request_id, derivation_path, system)
         VALUES ('deadline-request', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fixture.drv', 'x86_64-linux');
         CREATE FUNCTION delay_lease() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(3); RETURN NEW; END $$;
         CREATE TRIGGER delay_lease BEFORE INSERT ON store_leases
         FOR EACH ROW EXECUTE FUNCTION delay_lease();",
    ).unwrap();
    let database = fixture.database().for_request(
        std::time::Instant::now() + Duration::from_millis(300),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let started = std::time::Instant::now();
    let result = telchar::persistence::create_request_input_leases_with_limit(
        &database,
        "deadline-request",
        u64::MAX,
        &[(
            "deadline-lease".to_owned(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input".to_owned(),
            1,
        )],
    );
    assert!(
        result.is_err(),
        "slow admission must exceed request deadline"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        let running: i64 = fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE pid <> pg_backend_pid()
             AND datname = current_database() AND state = 'active' AND wait_event = 'PgSleep'",
                &[],
            )
            .unwrap()
            .get(0);
        if running == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed-out request left server work running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let persisted: i64 = fixture
        .connect()
        .query_one(
            "SELECT count(*) FROM store_leases WHERE lease_id = 'deadline-lease'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(persisted, 0, "timed-out lease must not be committed");
}
