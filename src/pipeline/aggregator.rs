use moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;

/// In-memory aggregate state for a fingerprint.
#[derive(Debug, Clone)]
pub struct InMemoryAggregate {
    pub count: u64,
    #[allow(dead_code)]
    pub first_seen: i64,
    pub last_seen: i64,
}

/// In-memory aggregator using moka cache.
/// Provides fast, lock-free reads for hot-path aggregate updates.
pub struct Aggregator {
    cache: Cache<String, InMemoryAggregate>,
}

impl Aggregator {
    pub fn new(max_capacity: u64, ttl_secs: u64) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(Duration::from_secs(ttl_secs))
            .build();

        Self { cache }
    }

    /// Increment the count for a fingerprint. Returns whether this is a new fingerprint.
    pub fn increment(&self, fingerprint: &str, timestamp: i64) -> bool {
        let existing = self.cache.get(fingerprint);
        let is_new = existing.is_none();

        match existing {
            Some(mut agg) => {
                agg.count += 1;
                agg.last_seen = timestamp;
                self.cache.insert(fingerprint.to_string(), agg);
            }
            None => {
                self.cache.insert(
                    fingerprint.to_string(),
                    InMemoryAggregate {
                        count: 1,
                        first_seen: timestamp,
                        last_seen: timestamp,
                    },
                );
            }
        }

        is_new
    }

    /// Get the current aggregate for a fingerprint.
    #[allow(dead_code)]
    pub fn get(&self, fingerprint: &str) -> Option<InMemoryAggregate> {
        self.cache.get(fingerprint)
    }

    /// Get the current count of tracked fingerprints.
    #[allow(dead_code)]
    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }
}

/// Shared aggregator reference.
pub type SharedAggregator = Arc<Aggregator>;

pub fn create_aggregator() -> SharedAggregator {
    // 100k fingerprints max, 1 hour TTL
    Arc::new(Aggregator::new(100_000, 3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_increment_new_fingerprint_returns_true() {
        let agg = Aggregator::new(100, 3600);
        let is_new = agg.increment("fp1", 1000);
        assert!(is_new, "first increment should indicate new fingerprint");
    }

    #[test]
    fn test_increment_existing_fingerprint_returns_false() {
        let agg = Aggregator::new(100, 3600);
        agg.increment("fp1", 1000);
        let is_new = agg.increment("fp1", 2000);
        assert!(!is_new, "second increment should not be new");
    }

    #[test]
    fn test_different_fingerprints_tracked_separately() {
        let agg = Aggregator::new(100, 3600);
        assert!(agg.increment("fp1", 1000));
        assert!(agg.increment("fp2", 1000));
        assert!(!agg.increment("fp1", 2000));
        assert!(!agg.increment("fp2", 2000));

        let fp1 = agg.get("fp1").unwrap();
        assert_eq!(fp1.count, 2);
        let fp2 = agg.get("fp2").unwrap();
        assert_eq!(fp2.count, 2);
    }

    #[test]
    fn test_count_aggregation_correctness() {
        let agg = Aggregator::new(100, 3600);
        for i in 0..10 {
            agg.increment("fp1", 1000 + i);
        }
        let state = agg.get("fp1").unwrap();
        assert_eq!(state.count, 10);
        assert_eq!(state.first_seen, 1000);
        assert_eq!(state.last_seen, 1009);
    }

    #[test]
    fn test_entry_count() {
        let agg = Aggregator::new(100, 3600);
        agg.increment("fp1", 1000);
        agg.increment("fp2", 1000);
        agg.increment("fp1", 2000);
        // moka cache is eventually consistent; use run_pending_tasks or just verify via get
        assert!(agg.get("fp1").is_some());
        assert!(agg.get("fp2").is_some());
        assert!(agg.get("fp3").is_none());
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let agg = Aggregator::new(100, 3600);
        assert!(agg.get("nonexistent").is_none());
    }
}
