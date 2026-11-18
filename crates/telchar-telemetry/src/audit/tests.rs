//! Shutdown fences are independent of audit queue capacity.

use super::*;

#[derive(Debug)]
struct Ordinary(Arc<AtomicBool>);
impl LogProcessor for Ordinary {
    fn emit(&self, _: &mut SdkLogRecord, _: &InstrumentationScope) {}
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        self.0.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Debug)]
struct SlowExporter(Arc<AtomicBool>);
impl LogExporter for SlowExporter {
    async fn export(&self, _: LogBatch<'_>) -> OTelSdkResult {
        self.0.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}

#[test]
fn full_audit_queue_cannot_skip_ordinary_shutdown_or_worker_stop() {
    let shutdown = Arc::new(AtomicBool::new(false));
    let exporting = Arc::new(AtomicBool::new(false));
    let processor = AuditProcessor::new(
        Ordinary(Arc::clone(&shutdown)),
        Arc::new(SlowExporter(Arc::clone(&exporting))),
    );
    let scope = InstrumentationScope::builder("test").build();
    let logger = SdkLoggerProvider::builder().build().logger("test");
    let mut record = logger.create_log_record();
    record.set_target("telchar.audit");
    processor.emit(&mut record, &scope);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !exporting.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    for _ in 0..super::super::MAX_QUEUE_SIZE {
        processor.emit(&mut record, &scope);
    }
    assert!(
        processor
            .shutdown_with_timeout(Duration::from_millis(20))
            .is_err()
    );
    assert!(
        shutdown.load(Ordering::Acquire),
        "ordinary shutdown skipped"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if processor
            .worker
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_finished()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audit worker ignored stop fence"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
