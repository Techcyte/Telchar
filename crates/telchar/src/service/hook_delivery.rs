//! Protects hook delivery outputs with attempt-scoped durable GC roots.

use std::io;

use crate::persistence::{self, DatabaseSource, PostBuildHookRoot};
use crate::store::retention::{ReleasedRetentionEntry, RetentionEntry, StoreRetentionBackend};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HookDeliveryPolicy {
    pub concurrency: usize,
    pub maximum_attempts: i32,
    pub maximum_age: std::time::Duration,
    pub initial_backoff: std::time::Duration,
    pub maximum_backoff: std::time::Duration,
}

impl Default for HookDeliveryPolicy {
    fn default() -> Self {
        Self {
            concurrency: 1,
            maximum_attempts: 32,
            maximum_age: std::time::Duration::from_secs(86_400),
            initial_backoff: std::time::Duration::from_secs(5),
            maximum_backoff: std::time::Duration::from_secs(3600),
        }
    }
}

impl HookDeliveryPolicy {
    pub fn validate(self) -> io::Result<Self> {
        if !(1..=16).contains(&self.concurrency)
            || !(1..=1000).contains(&self.maximum_attempts)
            || self.maximum_age.is_zero()
            || self.maximum_age.as_secs() > 604_800
            || self.initial_backoff.is_zero()
            || self.initial_backoff > self.maximum_backoff
            || self.maximum_backoff.as_secs() > 86_400
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid hook delivery bounds",
            ));
        }
        Ok(self)
    }
}

pub fn prepare_outputs(
    database: &(impl DatabaseSource + ?Sized),
    derivation_path: &str,
    retention: &mut dyn StoreRetentionBackend,
    paths: &[String],
) -> io::Result<()> {
    let _lock = retention.hook_delivery_lock()?;
    let roots = persistence::prepare_post_build_hook_roots(database, derivation_path)
        .map_err(io::Error::other)?;
    for root in roots.iter().filter(|root| paths.contains(&root.store_path)) {
        retention.retain(&[RetentionEntry::new(root.lease_id(), &root.store_path)])?;
        persistence::retain_post_build_hook_root(database, root).map_err(io::Error::other)?;
    }
    Ok(())
}

pub fn run_attempt(
    database: &(impl DatabaseSource + ?Sized),
    hook: &crate::service::post_build_hook::PostBuildHook,
    claim_duration: std::time::Duration,
    policy: HookDeliveryPolicy,
    mut store: Option<&mut dyn crate::store::query::QueryValidPathsStore>,
) -> io::Result<bool> {
    let policy = policy.validate()?;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let token = format!(
        "hook_delivery-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );
    let Some(hook_delivery) = persistence::claim_post_build_hooks(database, &token, claim_duration)
        .map_err(io::Error::other)?
    else {
        return Ok(false);
    };
    database
        .database()
        .map_err(io::Error::other)?
        .wait_for_ownership()?;
    if hook_delivery.attempt_count > policy.maximum_attempts
        || hook_delivery.age >= policy.maximum_age
    {
        if persistence::abandon_post_build_hooks(database, &hook_delivery, "retry-exhausted")
            .map_err(io::Error::other)?
        {
            crate::service::metrics::hook_delivery_abandoned("retry-exhausted");
        }
        return Ok(true);
    }
    if let Some(store) = store.as_mut() {
        let paths = hook_delivery
            .output_paths
            .iter()
            .map(|path| path.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let valid = store.query_valid_paths(&paths, false)?;
        if paths.iter().any(|path| !valid.contains(path)) {
            if persistence::abandon_post_build_hooks(database, &hook_delivery, "missing-path")
                .map_err(io::Error::other)?
            {
                crate::service::metrics::hook_delivery_abandoned("missing-path");
            }
            return Ok(true);
        }
    }
    let started = std::time::Instant::now();
    let result = hook.run(&hook_delivery.output_paths);
    crate::service::metrics::post_build_hook_finished(
        started.elapsed(),
        if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        },
    );
    match result {
        Ok(()) => {
            persistence::acknowledge_post_build_hooks(database, &hook_delivery)
                .map_err(io::Error::other)?;
        }
        Err(error) => {
            tracing::warn!(event = "post_build_hooks.attempt_failed", attempt = hook_delivery.attempt_count,
                failure_class = "hook-failed", diagnostic = %error, "post-build hook delivery failed");
            persistence::retry_post_build_hooks(
                database,
                &hook_delivery,
                "hook-failed",
                policy
                    .initial_backoff
                    .saturating_mul(
                        1_u32 << (hook_delivery.attempt_count.saturating_sub(1).min(16) as u32),
                    )
                    .min(policy.maximum_backoff),
            )
            .map_err(io::Error::other)?;
        }
    }
    Ok(true)
}

pub fn reconcile_pending_roots(
    database: &(impl DatabaseSource + ?Sized),
    retention: &mut dyn StoreRetentionBackend,
    mut query: Option<&mut dyn crate::store::query::QueryValidPathsStore>,
) -> io::Result<()> {
    if persistence::read_pending_post_build_hook_roots(database, None)
        .map_err(io::Error::other)?
        .is_empty()
    {
        return Ok(());
    }
    let _lock = retention.hook_delivery_lock()?;
    let mut after = None;
    loop {
        let roots = persistence::read_pending_post_build_hook_roots(database, after)
            .map_err(io::Error::other)?;
        if roots.is_empty() {
            return Ok(());
        }
        for root in &roots {
            if let Err(error) =
                retention.retain(&[RetentionEntry::new(root.lease_id(), &root.store_path)])
            {
                let Some(query) = query.as_mut() else {
                    return Err(error);
                };
                let paths = [root.store_path.as_bytes().to_vec()];
                if !query.query_valid_paths(&paths, false)?.is_empty() {
                    return Err(error);
                }
                if persistence::abandon_missing_post_build_hooks(database, root.attempt_id)
                    .map_err(io::Error::other)?
                {
                    crate::service::metrics::hook_delivery_abandoned("missing-path");
                }
            }
        }
        after = roots
            .last()
            .map(|root| (root.attempt_id, root.output_index));
    }
}

pub fn reconcile_released_roots(
    database: &(impl DatabaseSource + ?Sized),
    retention: &mut dyn StoreRetentionBackend,
) -> io::Result<()> {
    if persistence::read_post_build_hook_root_cleanup(database, 1)
        .map_err(io::Error::other)?
        .is_empty()
    {
        persistence::prune_post_build_hooks(database).map_err(io::Error::other)?;
        return Ok(());
    }
    let _lock = retention.hook_delivery_lock()?;
    let roots =
        persistence::read_post_build_hook_root_cleanup(database, 256).map_err(io::Error::other)?;
    for root in roots {
        release_root(database, retention, &root)?;
    }
    persistence::prune_post_build_hooks(database).map_err(io::Error::other)?;
    Ok(())
}

fn release_root(
    database: &(impl DatabaseSource + ?Sized),
    retention: &mut dyn StoreRetentionBackend,
    root: &PostBuildHookRoot,
) -> io::Result<()> {
    database
        .database()
        .map_err(io::Error::other)?
        .wait_for_ownership()?;
    retention.release(&[ReleasedRetentionEntry::new(
        root.lease_id(),
        &root.store_path,
    )])?;
    persistence::release_post_build_hook_root(database, root).map_err(io::Error::other)?;
    Ok(())
}
