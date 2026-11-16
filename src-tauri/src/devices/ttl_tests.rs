#[cfg(test)]
mod tests {
    use crate::devices::ttl::{TtlConfig, TtlDevice};
    use crate::devices::{Device, DeviceConfig, DeviceError, DeviceStatus, DeviceType};

    #[test]
    fn test_ttl_config_default() {
        let config = TtlConfig::default();
        assert_eq!(config.port_name, "");
        assert_eq!(config.baud_rate, 115200);
        assert_eq!(config.pulse_duration_ms, 10);
    }

    #[test]
    fn test_ttl_device_new() {
        let device = TtlDevice::new("/dev/ttyUSB0".to_string());
        assert_eq!(device.port_name, "/dev/ttyUSB0");
        assert_eq!(device.status, DeviceStatus::Disconnected);
        assert_eq!(device.config.port_name, "/dev/ttyUSB0");
        assert_eq!(device.config.baud_rate, 115200);
    }

    #[tokio::test]
    async fn test_ttl_device_info() {
        let device = TtlDevice::new("/dev/ttyUSB0".to_string());
        let info = device.get_info();

        assert!(info.id.starts_with("ttl_"));
        assert!(info.name.contains("TTL Pulse Generator"));
        assert_eq!(info.device_type, DeviceType::TTL);
        assert_eq!(info.status, DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_ttl_device_status() {
        let device = TtlDevice::new("/dev/ttyUSB0".to_string());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[test]
    fn test_ttl_list_ports() {
        // This test might fail if no serial ports are available
        // but it tests the function doesn't panic
        let result = TtlDevice::list_ports();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_connect_without_port() {
        let mut device = TtlDevice::new("/dev/nonexistent".to_string());
        let result = device.connect().await;

        // Should fail since port doesn't exist
        assert!(result.is_err());
        // Status should be Error after failed connection
        assert!(matches!(
            device.status,
            DeviceStatus::Disconnected | DeviceStatus::Error
        ));
    }

    #[tokio::test]
    async fn test_disconnect_when_not_connected() {
        let mut device = TtlDevice::new("/dev/ttyUSB0".to_string());
        let result = device.disconnect().await;

        // Should succeed even when not connected
        assert!(result.is_ok());
        assert_eq!(device.status, DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_send_when_disconnected() {
        let mut device = TtlDevice::new("/dev/ttyUSB0".to_string());
        let result = device.send(b"PULSE\n").await;

        // Should fail when not connected
        assert!(result.is_err());
        match result {
            Err(DeviceError::NotConnected) => (),
            _ => panic!("Expected NotConnected error"),
        }
    }

    #[test]
    fn test_device_config() {
        let mut device = TtlDevice::new("/dev/ttyUSB0".to_string());

        let config = DeviceConfig {
            auto_reconnect: true,
            reconnect_interval_ms: 5000,
            timeout_ms: 1000,
            custom_settings: serde_json::json!({
                "port_name": "/dev/ttyUSB1",
                "baud_rate": 9600,
                "pulse_duration_ms": 50
            }),
        };

        let result = device.configure(config.clone());
        assert!(result.is_ok());

        // Verify configuration was applied
        assert_eq!(device.config.port_name, "/dev/ttyUSB1");
        assert_eq!(device.config.baud_rate, 9600);
        assert_eq!(device.config.pulse_duration_ms, 50);
    }

    #[test]
    fn test_device_config_pulse_duration_default_preserved() {
        let mut device = TtlDevice::new("/dev/ttyUSB0".to_string());
        assert_eq!(device.config.pulse_duration_ms, 10);

        // Configure without pulse_duration_ms — default should be preserved
        let config = DeviceConfig {
            custom_settings: serde_json::json!({
                "port_name": "/dev/ttyUSB1"
            }),
            ..Default::default()
        };

        let result = device.configure(config);
        assert!(result.is_ok());
        assert_eq!(device.config.pulse_duration_ms, 10);
    }

    #[tokio::test]
    async fn test_device_trait_implementation() {
        let device = TtlDevice::new("/dev/ttyUSB0".to_string());

        // Verify Device trait is properly implemented
        let _info = device.get_info();
        let _status = device.get_status();
    }

    #[test]
    fn test_debug_implementation() {
        let device = TtlDevice::new("/dev/ttyUSB0".to_string());
        let debug_str = format!("{:?}", device);

        // Verify debug output contains expected fields
        assert!(debug_str.contains("TtlDevice"));
        assert!(debug_str.contains("port_name"));
        assert!(debug_str.contains("/dev/ttyUSB0"));
        assert!(debug_str.contains("status"));
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::devices::ttl::TtlDevice;
    use crate::devices::Device;
    use std::time::Duration;
    use tokio::time::timeout;

    // Mock serial port for testing
    #[cfg(target_os = "macos")]
    const TEST_PORT: &str = "/dev/tty.usbmodem14101";
    #[cfg(target_os = "linux")]
    const TEST_PORT: &str = "/dev/ttyUSB0";
    #[cfg(target_os = "windows")]
    const TEST_PORT: &str = "COM3";

    #[tokio::test]
    #[ignore] // Ignore by default since it requires hardware
    async fn test_real_device_connection() {
        let mut device = TtlDevice::new(TEST_PORT.to_string());

        // Attempt to connect
        let connect_result = timeout(Duration::from_secs(5), device.connect()).await;

        if let Ok(Ok(())) = connect_result {
            // If connected, test sending a pulse
            let send_result = device.send(b"PULSE\n").await;
            assert!(send_result.is_ok());

            // Disconnect
            let disconnect_result = device.disconnect().await;
            assert!(disconnect_result.is_ok());
        } else {
            // Hardware not available, skip test
            println!("TTL hardware not available, skipping integration test");
        }
    }

    #[tokio::test]
    async fn test_concurrent_operations() {
        use tokio::task;

        // Test that multiple TTL devices can be created
        let handles: Vec<_> = (0..3)
            .map(|i| {
                task::spawn(async move {
                    let device = TtlDevice::new(format!("/dev/tty{}", i));
                    device.get_info()
                })
            })
            .collect();

        for handle in handles {
            let info = handle.await.unwrap();
            assert!(info.id.starts_with("ttl_"));
        }
    }
}
