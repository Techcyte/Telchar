//! Tests background lifecycle-detail retention with database restart.

use super::*;

#[test]
fn maintenance_prunes_eligible_details_after_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.database()).unwrap();
    fixture.connect().batch_execute("
        INSERT INTO protocol_sessions (session_id, requester_reference, state, created_at, closed_at)
        VALUES ('maintenance-history', 'fixture', 'closed', now() - interval '60 days', now() - interval '40 days');
        INSERT INTO store_leases (lease_id, owner_kind, owner_id, store_path, purpose, state, nar_size, created_at, released_at, reconciled_at)
        SELECT 'maintenance-' || n, 'session', 'maintenance-history', '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-input', 'input', 'reconciled', 1,
            now() - interval '60 days', now() - interval '50 days',
            CASE WHEN n = 258 THEN now() - interval '1 day' ELSE now() - interval '40 days' END
        FROM generate_series(1, 258) n;
    ").unwrap();
    fixture.restart();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .unwrap(),
    );
    let database = owner.database().clone();
    let mut cursor = telchar::persistence::LeaseDetailCursor::default();
    let mut service = telchar::service::daemon_services::MaintenanceService::start_with_ownership(
        Duration::from_millis(10),
        owner,
        move || {
            telchar::persistence::prune_store_lease_details(
                &database,
                Duration::from_secs(30 * 86400),
                256,
                &mut cursor,
            )
            .map(|_| ())
            .map_err(std::io::Error::other)
        },
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        service.check().unwrap();
        let count: i64 = fixture
            .connect()
            .query_one("SELECT count(*) FROM store_leases", &[])
            .unwrap()
            .get(0);
        if count == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "eligible details remain: {count}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    service.shutdown().unwrap();
    assert!(
        telchar::persistence::read_store_lease(fixture.database(), "maintenance-258")
            .unwrap()
            .is_some()
    );
}
