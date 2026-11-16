use std::time::{SystemTime, UNIX_EPOCH};

// Import real LSL library for local_clock()
use lsl;

/// LSL clock access for the inlets and outlets
#[derive(Debug)]
pub struct TimeSync {
    /// Use the LSL clock (otherwise system time)
    enabled: bool,
}

impl TimeSync {
    /// Create new time synchronization instance
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Get current LSL time using real lsl::local_clock()
    pub fn lsl_time(&self) -> f64 {
        if !self.enabled {
            return self.system_time();
        }

        // Use real LSL local clock - this is a simple FFI call that's safe to call
        // from any thread (doesn't involve Rc-based objects)
        lsl::local_clock()
    }

    /// Get current system time as seconds since epoch
    fn system_time(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    /// Create a timestamp for data samples
    pub fn create_timestamp(&self) -> f64 {
        self.lsl_time()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_time_sync_creation() {
        let sync = TimeSync::new(true);
        assert!(sync.enabled);
    }

    #[test]
    fn test_lsl_time_enabled() {
        // Test that lsl_time() returns real LSL clock when enabled
        let sync = TimeSync::new(true);
        let lsl_time = sync.lsl_time();

        // LSL time should be a positive value (time since LSL library init)
        assert!(lsl_time > 0.0);

        // Should be reasonably close to what lsl::local_clock() returns
        let direct_lsl = lsl::local_clock();
        assert!((lsl_time - direct_lsl).abs() < 1.0); // Within 1 second
    }

    #[test]
    fn test_timestamp_creation() {
        // Test that create_timestamp() produces valid LSL timestamps
        let sync = TimeSync::new(true);

        let ts1 = sync.create_timestamp();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let ts2 = sync.create_timestamp();

        // Timestamps should be positive and increasing
        assert!(ts1 > 0.0);
        assert!(ts2 > ts1);
    }
}
