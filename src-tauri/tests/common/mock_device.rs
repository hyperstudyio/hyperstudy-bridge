//! Test mock device implementation
//!
//! Provides a configurable mock device for testing that implements
//! the Device trait with controllable latency, error rates, and data tracking.

use async_trait::async_trait;
use hyperstudy_bridge::devices::{
    Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus, DeviceType,
};
use rand::Rng;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

/// Mock device implementation for testing
#[derive(Debug, Clone)]
pub struct TestMockDevice {
    pub id: String,
    pub name: String,
    pub device_type: DeviceType,
    pub status: Arc<RwLock<DeviceStatus>>,
    pub config: Arc<RwLock<DeviceConfig>>,
    pub sent_data: Arc<RwLock<Vec<Vec<u8>>>>,
    pub received_data: Arc<RwLock<Vec<Vec<u8>>>>,
    pub connection_delay: Duration,
    pub operation_delay: Duration,
    pub should_fail: Arc<AtomicBool>,
    pub error_rate: f64,
    pub latency_ms: u64,
    pub last_operation: Arc<Mutex<Option<Instant>>>,
}

impl TestMockDevice {
    /// Create a new mock device with default settings
    ///
    /// By default, the mock device has zero latency for fast tests.
    /// Use `with_latency()` to simulate real device latency.
    pub fn new(id: String, name: String, device_type: DeviceType) -> Self {
        Self {
            id,
            name,
            device_type,
            status: Arc::new(RwLock::new(DeviceStatus::Disconnected)),
            config: Arc::new(RwLock::new(DeviceConfig::default())),
            sent_data: Arc::new(RwLock::new(Vec::new())),
            received_data: Arc::new(RwLock::new(Vec::new())),
            connection_delay: Duration::from_micros(100), // Very fast for tests
            operation_delay: Duration::from_micros(10),   // Very fast for tests
            should_fail: Arc::new(AtomicBool::new(false)),
            error_rate: 0.0,
            latency_ms: 0,
            last_operation: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the operation latency
    pub fn with_latency(mut self, latency_ms: u64) -> Self {
        self.latency_ms = latency_ms;
        self.operation_delay = Duration::from_millis(latency_ms);
        self
    }

    /// Set the error rate (0.0 to 1.0)
    pub fn with_error_rate(mut self, error_rate: f64) -> Self {
        self.error_rate = error_rate.clamp(0.0, 1.0);
        self
    }

    /// Set the connection delay
    pub fn with_connection_delay(mut self, delay: Duration) -> Self {
        self.connection_delay = delay;
        self
    }

    /// Force all operations to fail
    pub fn set_should_fail(&self, should_fail: bool) {
        self.should_fail.store(should_fail, Ordering::Relaxed);
    }

    /// Get all data that has been "sent" to this device
    pub async fn get_sent_data(&self) -> Vec<Vec<u8>> {
        self.sent_data.read().await.clone()
    }

    /// Get all data that has been added to the receive buffer
    pub async fn get_received_data(&self) -> Vec<Vec<u8>> {
        self.received_data.read().await.clone()
    }

    /// Add data to the receive buffer (for testing receive operations)
    pub async fn add_received_data(&self, data: Vec<u8>) {
        self.received_data.write().await.push(data);
    }

    /// Clear all sent and received data
    pub async fn clear_data(&self) {
        self.sent_data.write().await.clear();
        self.received_data.write().await.clear();
    }

    /// Get the number of bytes sent
    pub async fn bytes_sent(&self) -> usize {
        self.sent_data.read().await.iter().map(|d| d.len()).sum()
    }

    /// Get the number of messages sent
    pub async fn messages_sent(&self) -> usize {
        self.sent_data.read().await.len()
    }

    /// Check if an operation should fail based on error rate
    fn should_simulate_error(&self) -> bool {
        if self.should_fail.load(Ordering::Relaxed) {
            return true;
        }

        if self.error_rate > 0.0 {
            let mut rng = rand::thread_rng();
            rng.gen::<f64>() < self.error_rate
        } else {
            false
        }
    }
}

#[async_trait]
impl Device for TestMockDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        if self.should_simulate_error() {
            return Err(DeviceError::ConnectionFailed(
                "Simulated connection failure".to_string(),
            ));
        }

        tokio::time::sleep(self.connection_delay).await;
        *self.status.write().await = DeviceStatus::Connected;
        *self.last_operation.lock().await = Some(Instant::now());
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        if self.should_simulate_error() {
            return Err(DeviceError::CommunicationError(
                "Simulated disconnect failure".to_string(),
            ));
        }

        tokio::time::sleep(Duration::from_millis(5)).await;
        *self.status.write().await = DeviceStatus::Disconnected;
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        if self.should_simulate_error() {
            return Err(DeviceError::CommunicationError(
                "Simulated send failure".to_string(),
            ));
        }

        let status = *self.status.read().await;
        if status != DeviceStatus::Connected {
            return Err(DeviceError::NotConnected);
        }

        tokio::time::sleep(self.operation_delay).await;
        self.sent_data.write().await.push(data.to_vec());
        *self.last_operation.lock().await = Some(Instant::now());
        Ok(())
    }

    fn get_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            device_type: self.device_type,
            status: self
                .status
                .try_read()
                .map(|guard| *guard)
                .unwrap_or(DeviceStatus::Error),
            metadata: json!({
                "latency_ms": self.latency_ms,
                "error_rate": self.error_rate,
                "test_device": true
            }),
        }
    }

    fn get_status(&self) -> DeviceStatus {
        self.status
            .try_read()
            .map(|guard| *guard)
            .unwrap_or(DeviceStatus::Error)
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        if self.should_simulate_error() {
            return Err(DeviceError::ConfigurationError(
                "Simulated configuration failure".to_string(),
            ));
        }

        // Use try_write to avoid blocking - this is fine for tests
        // If we can't get the lock, we just fail - this shouldn't happen in tests
        if let Ok(mut guard) = self.config.try_write() {
            *guard = config;
            Ok(())
        } else {
            Err(DeviceError::ConfigurationError(
                "Could not acquire config lock".to_string(),
            ))
        }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
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
    async fn test_mock_device_should_fail_flag() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        );

        // Should succeed initially
        assert!(device.connect().await.is_ok());
        device.disconnect().await.ok();

        // Set should_fail
        device.set_should_fail(true);
        assert!(device.connect().await.is_err());

        // Clear should_fail
        device.set_should_fail(false);
        assert!(device.connect().await.is_ok());
    }

    #[tokio::test]
    async fn test_mock_device_send_when_disconnected() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        );

        // Should fail when disconnected
        let result = device.send(b"test").await;
        assert!(matches!(result, Err(DeviceError::NotConnected)));
    }

    #[tokio::test]
    async fn test_mock_device_latency() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        )
        .with_latency(50); // 50ms latency

        device.connect().await.unwrap();

        let start = Instant::now();
        device.send(b"test").await.unwrap();
        let duration = start.elapsed();

        assert!(duration >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_mock_device_data_tracking() {
        let mut device = TestMockDevice::new(
            "test_device".to_string(),
            "Test Device".to_string(),
            DeviceType::Mock,
        );

        device.connect().await.unwrap();

        device.send(b"message1").await.unwrap();
        device.send(b"message2").await.unwrap();
        device.send(b"message3").await.unwrap();

        assert_eq!(device.messages_sent().await, 3);
        assert_eq!(device.bytes_sent().await, 24); // 8 + 8 + 8

        device.clear_data().await;
        assert_eq!(device.messages_sent().await, 0);
    }
}
