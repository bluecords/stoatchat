use std::sync::RwLock;
use std::time::{Duration, Instant};

use iso8601_timestamp::Timestamp;
use once_cell::sync::Lazy;

use crate::{AbstractPolicyChange, Database};

/// How long a cached answer is trusted.
///
/// `calculate_server_permissions` runs on effectively every request, so this
/// cannot hit the database each time. Policies are inserted by hand and almost
/// never change, so a short staleness window costs nothing: the worst case is a
/// member keeps access for up to this long after a new policy is published.
const TTL: Duration = Duration::from_secs(30);

static CACHE: Lazy<RwLock<Option<(Instant, Timestamp)>>> = Lazy::new(|| RwLock::new(None));

/// Creation time of the most recent policy change, or the epoch if there are none.
///
/// Cached process-wide. A race between two callers is harmless - both fetch, both
/// write the same answer.
pub async fn latest_policy_change_time(db: &Database) -> Timestamp {
    if let Ok(guard) = CACHE.read() {
        if let Some((fetched_at, value)) = *guard {
            if fetched_at.elapsed() < TTL {
                return value;
            }
        }
    }

    let latest = db
        .fetch_policy_changes()
        .await
        .map(|policies| {
            policies
                .into_iter()
                .map(|policy| policy.created_time)
                .max()
                .unwrap_or(Timestamp::UNIX_EPOCH)
        })
        // On a database error, fail OPEN rather than locking every member out of
        // the platform because one query failed. A gate that turns a transient
        // database blip into a total outage is worse than one that briefly lets
        // an unconsented member through.
        .unwrap_or(Timestamp::UNIX_EPOCH);

    if let Ok(mut guard) = CACHE.write() {
        *guard = Some((Instant::now(), latest));
    }

    latest
}
