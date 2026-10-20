//! Anonymous indexer cooldowns, keyed only by provider origin. No wallet,
//! address, account or token is retained. A 429 applies across both chains.
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static COOLDOWNS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn origin(base: &str) -> Option<String> {
    Some(
        reqwest::Url::parse(base)
            .ok()?
            .origin()
            .ascii_serialization(),
    )
}

pub(crate) fn remaining(base: &str) -> Option<Duration> {
    let key = origin(base)?;
    let mut entries = COOLDOWNS.get_or_init(Default::default).lock().ok()?;
    let now = Instant::now();
    entries.retain(|_, until| *until > now);
    entries
        .get(&key)
        .map(|until| until.saturating_duration_since(now))
}

pub(crate) fn record(base: &str, headers: &reqwest::header::HeaderMap) -> Duration {
    let delay = super::coincube::client::parse_retry_after(headers)
        .max(Duration::from_secs(1))
        .min(Duration::from_secs(86400));
    if let Some(key) = origin(base) {
        if let Ok(mut entries) = COOLDOWNS.get_or_init(Default::default).lock() {
            let now = Instant::now();
            entries.retain(|_, until| *until > now);
            let until = now + delay;
            entries
                .entry(key)
                .and_modify(|old| *old = (*old).max(until))
                .or_insert(until);
        }
    }
    delay
}
