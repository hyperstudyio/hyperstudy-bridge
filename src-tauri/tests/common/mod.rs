//! Common test utilities and infrastructure
//!
//! This module provides the test infrastructure for integration tests.
//!
//! # New Infrastructure (Recommended)
//!
//! Use the new modular infrastructure via the `prelude`:
//!
//! ```ignore
//! use crate::common::prelude::*;
//!
//! #[tokio::test]
//! async fn test_example() -> TestResult<()> {
//!     let mut harness = TestHarness::new().await;
//!     let device_id = harness.add_connected_device(DeviceType::TTL).await?;
//!
//!     Assertions::assert_device_status(
//!         &harness, &device_id, DeviceStatus::Connected, "after connect"
//!     ).await?;
//!
//!     harness.cleanup().await
//! }
//! ```
//!
//! # Legacy Infrastructure (Deprecated)
//!
//! The old `TestFixture` is still available for backward compatibility but
//! should not be used for new tests. It has issues with Drop panics and
//! 'static closure bounds.

// New modular infrastructure
pub mod assertions;
pub mod concurrent;
pub mod harness;
pub mod mock_device;
pub mod websocket;

// Re-export new infrastructure for easy access
pub use assertions::Assertions;
pub use concurrent::{
    measure_throughput, measure_throughput_with_errors, run_concurrent, run_load_test,
    ConcurrentResult, LatencyStats,
};
pub use harness::{TestError, TestHarness, TestResult};
pub use mock_device::TestMockDevice;
pub use websocket::{connect_with_retry, TestWebSocketClient};

/// Prelude module for convenient imports
pub mod prelude {
    pub use super::assertions::Assertions;
    pub use super::concurrent::{
        measure_throughput, measure_throughput_with_errors, run_concurrent, run_load_test,
        ConcurrentResult, LatencyStats,
    };
    pub use super::harness::{TestError, TestHarness, TestResult};
    pub use super::mock_device::TestMockDevice;
    pub use super::websocket::{connect_with_retry, TestWebSocketClient};

    // Re-export legacy utilities that are still useful
    pub use super::MemoryTracker;
    pub use super::TestDataGenerator;

    // Re-export commonly used types from the main crate
    pub use hyperstudy_bridge::bridge::{AppState, BridgeCommand, BridgeResponse};
    pub use hyperstudy_bridge::devices::{
        Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType,
    };

    // Common standard library types
    pub use std::sync::Arc;
    pub use std::time::{Duration, Instant};
}

// ============================================================================
// LEGACY INFRASTRUCTURE (Deprecated - for backward compatibility only)
// ============================================================================

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use hyperstudy_bridge::bridge::{AppState, BridgeCommand, BridgeResponse, BridgeServer};
use hyperstudy_bridge::devices::{
    Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType,
};
use rand::Rng;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Test data generator for various scenarios
pub struct TestDataGenerator {
    rng: rand::rngs::ThreadRng,
}

impl TestDataGenerator {
    pub fn new() -> Self {
        Self {
            rng: rand::thread_rng(),
        }
    }

    pub fn generate_device_id(&mut self) -> String {
        format!("test_device_{}", Uuid::new_v4())
    }

    pub fn generate_ttl_command(&mut self) -> BridgeCommand {
        BridgeCommand::Command {
            device: "ttl".to_string(),
            action: hyperstudy_bridge::bridge::message::CommandAction::Send,
            payload: Some(json!({"command": "PULSE"})),
            id: Some(self.generate_request_id()),
        }
    }

    pub fn generate_connect_command(&mut self, device_type: &str) -> BridgeCommand {
        let payload = match device_type {
            "ttl" => Some(json!({"port": "/dev/ttyUSB0"})),
            "kernel" => Some(json!({"ip": "127.0.0.1"})),
            "pupil" => Some(json!({"url": "neon.local:8080"})),
            "mock" => Some(json!({})),
            _ => None,
        };

        BridgeCommand::Command {
            device: device_type.to_string(),
            action: hyperstudy_bridge::bridge::message::CommandAction::Connect,
            payload,
            id: Some(self.generate_request_id()),
        }
    }

    pub fn generate_disconnect_command(&mut self, device_type: &str) -> BridgeCommand {
        BridgeCommand::Command {
            device: device_type.to_string(),
            action: hyperstudy_bridge::bridge::message::CommandAction::Disconnect,
            payload: None,
            id: Some(self.generate_request_id()),
        }
    }

    pub fn generate_status_query(&mut self, device_type: &str) -> BridgeCommand {
        BridgeCommand::Command {
            device: device_type.to_string(),
            action: hyperstudy_bridge::bridge::message::CommandAction::Status,
            payload: None,
            id: Some(self.generate_request_id()),
        }
    }

    pub fn generate_random_data(&mut self, size: usize) -> Vec<u8> {
        (0..size).map(|_| self.rng.gen()).collect()
    }

    pub fn generate_request_id(&mut self) -> String {
        Uuid::new_v4().to_string()
    }

    pub fn generate_large_message(&mut self, size_kb: usize) -> Value {
        let data = "x".repeat(size_kb * 1024);
        json!({"large_data": data})
    }
}

/// Performance measurement utilities
pub struct PerformanceMeasurement {
    pub start_time: Instant,
    pub end_time: Option<Instant>,
    pub operation_name: String,
}

impl PerformanceMeasurement {
    pub fn start(operation_name: String) -> Self {
        Self {
            start_time: Instant::now(),
            end_time: None,
            operation_name,
        }
    }

    pub fn stop(&mut self) -> Duration {
        self.end_time = Some(Instant::now());
        self.duration()
    }

    pub fn duration(&self) -> Duration {
        if let Some(end) = self.end_time {
            end.duration_since(self.start_time)
        } else {
            Instant::now().duration_since(self.start_time)
        }
    }

    pub fn duration_ms(&self) -> f64 {
        self.duration().as_secs_f64() * 1000.0
    }

    pub fn duration_ns(&self) -> u128 {
        self.duration().as_nanos()
    }

    pub fn is_within_threshold(&self, threshold_ms: f64) -> bool {
        self.duration_ms() <= threshold_ms
    }
}

/// Memory leak detection utilities
pub struct MemoryTracker {
    initial_memory: u64,
    peak_memory: u64,
    measurements: Vec<(Instant, u64)>,
}

impl MemoryTracker {
    pub fn new() -> Self {
        let initial = Self::get_memory_usage();
        Self {
            initial_memory: initial,
            peak_memory: initial,
            measurements: vec![(Instant::now(), initial)],
        }
    }

    pub fn measure(&mut self) {
        let current = Self::get_memory_usage();
        self.peak_memory = self.peak_memory.max(current);
        self.measurements.push((Instant::now(), current));
    }

    pub fn get_memory_usage() -> u64 {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let pid = Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_memory(),
        );
        system.process(pid).map(|p| p.memory()).unwrap_or(0)
    }

    pub fn memory_increase(&self) -> u64 {
        self.peak_memory.saturating_sub(self.initial_memory)
    }

    pub fn has_memory_leak(&self, threshold_mb: u64) -> bool {
        let increase_mb = self.memory_increase() / 1024 / 1024;
        increase_mb > threshold_mb
    }

    pub fn get_measurements(&self) -> &[(Instant, u64)] {
        &self.measurements
    }
}

/// DEPRECATED: Test fixture setup and teardown utilities
///
/// Use `TestHarness` instead for new tests. This struct has issues with
/// Drop panics when async cleanup fails.
#[deprecated(note = "Use TestHarness instead - it has explicit async cleanup")]
pub struct TestFixture {
    pub app_state: Arc<AppState>,
    pub temp_devices: Vec<String>,
    pub temp_files: Vec<String>,
}

#[allow(deprecated)]
impl TestFixture {
    pub async fn new() -> Self {
        let app_state = Arc::new(AppState::new());

        Self {
            app_state,
            temp_devices: Vec::new(),
            temp_files: Vec::new(),
        }
    }

    pub async fn add_mock_device(&mut self, device_type: DeviceType) -> String {
        let device_id = format!("test_{:?}_{}", device_type, Uuid::new_v4());
        let device = TestMockDevice::new(
            device_id.clone(),
            format!("Test {:?} Device", device_type),
            device_type,
        );

        self.app_state
            .add_device(device_id.clone(), Box::new(device))
            .await;
        self.temp_devices.push(device_id.clone());
        device_id
    }

    pub async fn add_high_latency_device(
        &mut self,
        device_type: DeviceType,
        latency_ms: u64,
    ) -> String {
        let device_id = format!("test_{:?}_slow_{}", device_type, Uuid::new_v4());
        let device = TestMockDevice::new(
            device_id.clone(),
            format!("Slow Test {:?} Device", device_type),
            device_type,
        )
        .with_latency(latency_ms);

        self.app_state
            .add_device(device_id.clone(), Box::new(device))
            .await;
        self.temp_devices.push(device_id.clone());
        device_id
    }

    pub async fn add_unreliable_device(
        &mut self,
        device_type: DeviceType,
        error_rate: f64,
    ) -> String {
        let device_id = format!("test_{:?}_unreliable_{}", device_type, Uuid::new_v4());
        let device = TestMockDevice::new(
            device_id.clone(),
            format!("Unreliable Test {:?} Device", device_type),
            device_type,
        )
        .with_error_rate(error_rate);

        self.app_state
            .add_device(device_id.clone(), Box::new(device))
            .await;
        self.temp_devices.push(device_id.clone());
        device_id
    }

    pub async fn cleanup(&mut self) {
        for device_id in &self.temp_devices {
            self.app_state.remove_device(device_id).await;
        }
        self.temp_devices.clear();

        for file_path in &self.temp_files {
            let _ = tokio::fs::remove_file(file_path).await;
        }
        self.temp_files.clear();
    }

    pub async fn wait_for_device_status(
        &self,
        device_id: &str,
        expected_status: DeviceStatus,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();

        while start.elapsed() < timeout {
            if let Some(status) = self.app_state.get_device_status(device_id).await {
                if status == expected_status {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    pub async fn get_device_count(&self) -> usize {
        self.app_state.devices.read().await.len()
    }
}

#[allow(deprecated)]
impl Drop for TestFixture {
    fn drop(&mut self) {
        // DEPRECATED: This Drop implementation can panic when used in async contexts.
        // Use TestHarness with explicit cleanup() instead.
        tokio::task::block_in_place(|| {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.block_on(async {
                    self.cleanup().await;
                });
            }
        });
    }
}

/// Utility functions for common test operations
pub mod test_utils {
    use super::*;

    /// Wait for a condition to be true with timeout
    pub async fn wait_for_condition<F>(mut condition: F, timeout: Duration) -> bool
    where
        F: FnMut() -> bool,
    {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Wait for an async condition to be true with timeout
    pub async fn wait_for_async_condition<F, Fut>(mut condition: F, timeout: Duration) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if condition().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Create multiple mock devices of different types
    #[allow(deprecated)]
    pub async fn create_multi_device_setup(
        fixture: &mut TestFixture,
    ) -> HashMap<DeviceType, String> {
        let mut devices = HashMap::new();

        devices.insert(
            DeviceType::TTL,
            fixture.add_mock_device(DeviceType::TTL).await,
        );
        devices.insert(
            DeviceType::Kernel,
            fixture.add_mock_device(DeviceType::Kernel).await,
        );
        devices.insert(
            DeviceType::Pupil,
            fixture.add_mock_device(DeviceType::Pupil).await,
        );
        devices.insert(
            DeviceType::Mock,
            fixture.add_mock_device(DeviceType::Mock).await,
        );

        devices
    }

    /// Measure throughput of operations (DEPRECATED - use concurrent::measure_throughput)
    #[deprecated(note = "Use concurrent::measure_throughput instead")]
    pub async fn measure_throughput<F, Fut>(operation: F, duration: Duration) -> (u64, f64)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let start = Instant::now();
        let mut count = 0u64;

        while start.elapsed() < duration {
            operation().await;
            count += 1;
        }

        let actual_duration = start.elapsed().as_secs_f64();
        let throughput = count as f64 / actual_duration;

        (count, throughput)
    }

    /// Assert that TTL latency is under 1ms
    pub fn assert_ttl_latency_compliance(latency: Duration) {
        assert!(
            latency.as_millis() < 1,
            "TTL latency {} ms exceeds 1ms requirement",
            latency.as_millis()
        );
    }

    /// Assert that throughput meets requirements
    pub fn assert_throughput_compliance(throughput: f64, minimum: f64) {
        assert!(
            throughput >= minimum,
            "Throughput {} msg/sec is below minimum requirement of {} msg/sec",
            throughput,
            minimum
        );
    }

    /// Generate load test scenario (DEPRECATED - use concurrent::run_load_test)
    #[deprecated(note = "Use concurrent::run_load_test instead")]
    pub async fn run_load_test<F, Fut>(
        operation: F,
        concurrent_operations: usize,
        operations_per_worker: usize,
    ) -> Vec<Duration>
    where
        F: Fn(usize) -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Duration> + Send + 'static,
    {
        let mut handles = Vec::new();

        for worker_id in 0..concurrent_operations {
            let op = operation.clone();
            let handle = tokio::spawn(async move {
                let mut latencies = Vec::new();
                for _ in 0..operations_per_worker {
                    let latency = op(worker_id).await;
                    latencies.push(latency);
                }
                latencies
            });
            handles.push(handle);
        }

        let mut all_latencies = Vec::new();
        for handle in handles {
            let worker_latencies = handle.await.unwrap();
            all_latencies.extend(worker_latencies);
        }

        all_latencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_device_basic_operations() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        );

        // Test connection
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
        device.connect().await.unwrap();
        assert_eq!(device.get_status(), DeviceStatus::Connected);

        // Test send/receive
        let test_data = b"test_message";
        device.send(test_data).await.unwrap();
        let sent_data = device.get_sent_data().await;
        assert_eq!(sent_data.len(), 1);
        assert_eq!(sent_data[0], test_data);

        // Test disconnect
        device.disconnect().await.unwrap();
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_mock_device_error_simulation() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        )
        .with_error_rate(1.0); // 100% error rate

        // Should fail to connect
        assert!(device.connect().await.is_err());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_performance_measurement() {
        let mut measurement = PerformanceMeasurement::start("test_operation".to_string());

        tokio::time::sleep(Duration::from_millis(50)).await;
        let duration = measurement.stop();

        assert!(duration.as_millis() >= 50);
        assert!(measurement.is_within_threshold(100.0));
        assert!(!measurement.is_within_threshold(10.0));
    }

    #[tokio::test]
    #[allow(deprecated)]
    #[ignore = "TestFixture is deprecated and uses block_in_place which can panic in current_thread runtime"]
    async fn test_test_fixture() {
        let mut fixture = TestFixture::new().await;

        let device_id = fixture.add_mock_device(DeviceType::TTL).await;
        assert_eq!(fixture.get_device_count().await, 1);

        let status = fixture.app_state.get_device_status(&device_id).await;
        assert!(status.is_some());

        fixture.cleanup().await;
        assert_eq!(fixture.get_device_count().await, 0);
    }
}
