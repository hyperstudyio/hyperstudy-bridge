//! Test Harness - Core test infrastructure with explicit async cleanup
//!
//! This module provides the main test harness that replaces TestFixture.
//! Key differences from TestFixture:
//! - No Drop implementation - cleanup is always explicit and async
//! - Simple ownership model with guard patterns
//! - Better error types with context

use hyperstudy_bridge::bridge::AppState;
use hyperstudy_bridge::devices::{DeviceError, DeviceStatus, DeviceType};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

use super::mock_device::TestMockDevice;

/// Result type for test operations
pub type TestResult<T = ()> = Result<T, TestError>;

/// Test-specific error types with context
#[derive(Debug, Error)]
pub enum TestError {
    #[error("Device error: {0}")]
    Device(#[from] DeviceError),

    #[error("Setup failed: {0}")]
    Setup(String),

    #[error("Assertion failed: {0}")]
    Assertion(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Concurrent task failed: {0}")]
    TaskFailed(String),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Test harness for managing test state and devices
///
/// Unlike TestFixture, this struct does NOT implement Drop.
/// Cleanup must be called explicitly via `cleanup().await`.
///
/// # Example
/// ```ignore
/// #[tokio::test]
/// async fn test_example() -> TestResult<()> {
///     let mut harness = TestHarness::new().await;
///
///     let device_id = harness.add_connected_device(DeviceType::TTL).await?;
///
///     // ... test logic ...
///
///     harness.cleanup().await
/// }
/// ```
pub struct TestHarness {
    /// The application state containing devices
    pub app_state: Arc<AppState>,

    /// Performance monitor for metrics

    /// IDs of devices added by this harness (for cleanup)
    pub devices: Vec<String>,

    /// Temporary files to clean up
    temp_files: Vec<String>,
}

impl TestHarness {
    /// Create a new test harness
    pub async fn new() -> Self {
        let app_state = Arc::new(AppState::new());

        Self {
            app_state,
            devices: Vec::new(),
            temp_files: Vec::new(),
        }
    }

    /// Explicit async cleanup - MUST be called at end of test
    ///
    /// Returns Ok(()) always. Cleanup errors are ignored since they're often
    /// expected (e.g., disconnecting an already-disconnected device or
    /// an intentionally unreliable device).
    ///
    /// All cleanup operations are attempted even if some fail.
    pub async fn cleanup(mut self) -> TestResult<()> {
        // Disconnect and remove all devices
        // We ignore errors here since cleanup should be best-effort
        for device_id in &self.devices {
            // Try to disconnect first (ignore errors)
            if let Some(device_lock) = self.app_state.get_device(device_id).await {
                let mut device = device_lock.write().await;
                let _ = device.disconnect().await;
            }

            // Remove from state
            self.app_state.remove_device(device_id).await;
        }
        self.devices.clear();

        // Clean up temporary files (ignore errors)
        for file_path in &self.temp_files {
            let _ = tokio::fs::remove_file(file_path).await;
        }
        self.temp_files.clear();

        Ok(())
    }

    /// Add a mock device with the given type
    ///
    /// Returns the device ID. The device is in disconnected state.
    pub async fn add_device(&mut self, device_type: DeviceType) -> String {
        let device_id = format!("test_{:?}_{}", device_type, Uuid::new_v4());
        let device = TestMockDevice::new(
            device_id.clone(),
            format!("Test {:?} Device", device_type),
            device_type,
        );

        self.app_state
            .add_device(device_id.clone(), Box::new(device))
            .await;
        self.devices.push(device_id.clone());
        device_id
    }

    /// Add a mock device and connect it in one call
    ///
    /// Returns the device ID if successful.
    pub async fn add_connected_device(&mut self, device_type: DeviceType) -> TestResult<String> {
        let device_id = self.add_device(device_type).await;

        if let Some(device_lock) = self.app_state.get_device(&device_id).await {
            let mut device = device_lock.write().await;
            device.connect().await?;
        } else {
            return Err(TestError::Setup(format!(
                "Device {} not found after adding",
                device_id
            )));
        }

        Ok(device_id)
    }

    /// Add a device with configurable latency
    pub async fn add_device_with_latency(
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
        self.devices.push(device_id.clone());
        device_id
    }

    /// Add a device with configurable error rate
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
        self.devices.push(device_id.clone());
        device_id
    }

    /// Add a fully configured mock device
    pub async fn add_configured_device(
        &mut self,
        device_type: DeviceType,
        latency_ms: u64,
        error_rate: f64,
        connection_delay: Duration,
    ) -> String {
        let device_id = format!("test_{:?}_configured_{}", device_type, Uuid::new_v4());
        let device = TestMockDevice::new(
            device_id.clone(),
            format!("Configured Test {:?} Device", device_type),
            device_type,
        )
        .with_latency(latency_ms)
        .with_error_rate(error_rate)
        .with_connection_delay(connection_delay);

        self.app_state
            .add_device(device_id.clone(), Box::new(device))
            .await;
        self.devices.push(device_id.clone());
        device_id
    }

    /// Register a temporary file for cleanup
    pub fn register_temp_file(&mut self, path: String) {
        self.temp_files.push(path);
    }

    /// Get the current device count
    pub async fn device_count(&self) -> usize {
        self.app_state.devices.read().await.len()
    }

    /// Get device status with proper error handling
    pub async fn get_device_status(&self, device_id: &str) -> TestResult<DeviceStatus> {
        self.app_state
            .get_device_status(device_id)
            .await
            .ok_or_else(|| TestError::Setup(format!("Device {} not found", device_id)))
    }

    /// Add multiple devices of different types for multi-device testing
    pub async fn add_multi_device_setup(
        &mut self,
    ) -> std::collections::HashMap<DeviceType, String> {
        let mut devices = std::collections::HashMap::new();

        for device_type in [
            DeviceType::TTL,
            DeviceType::Kernel,
            DeviceType::Pupil,
            DeviceType::Mock,
        ] {
            let device_id = self.add_device(device_type).await;
            devices.insert(device_type, device_id);
        }

        devices
    }

    /// Add and connect multiple devices for multi-device testing
    pub async fn add_connected_multi_device_setup(
        &mut self,
    ) -> TestResult<std::collections::HashMap<DeviceType, String>> {
        let mut devices = std::collections::HashMap::new();

        for device_type in [
            DeviceType::TTL,
            DeviceType::Kernel,
            DeviceType::Pupil,
            DeviceType::Mock,
        ] {
            let device_id = self.add_connected_device(device_type).await?;
            devices.insert(device_type, device_id);
        }

        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_harness_basic_lifecycle() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_device(DeviceType::Mock).await;
        assert_eq!(harness.device_count().await, 1);

        let status = harness.get_device_status(&device_id).await?;
        assert_eq!(status, DeviceStatus::Disconnected);

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_harness_connected_device() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_connected_device(DeviceType::TTL).await?;
        let status = harness.get_device_status(&device_id).await?;
        assert_eq!(status, DeviceStatus::Connected);

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_harness_cleanup_removes_devices() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let app_state = harness.app_state.clone();

        let device_id = harness.add_device(DeviceType::Mock).await;

        // Device should exist
        assert!(app_state.get_device(&device_id).await.is_some());

        // Cleanup
        harness.cleanup().await?;

        // Device should be gone
        assert!(app_state.get_device(&device_id).await.is_none());

        Ok(())
    }
}
