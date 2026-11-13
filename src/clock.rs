//! The one place that reads the wall clock as Unix seconds. `App` stays pure (principle 1): it only takes the time from here for
//! values that are stored or compared with relay timestamps, never for decisions that a test could not set up with an event.

/// Unix seconds now; 0 if the clock is before the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
