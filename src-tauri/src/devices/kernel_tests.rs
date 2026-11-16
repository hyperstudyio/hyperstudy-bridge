#[cfg(test)]
mod tests {
    use crate::devices::kernel::{KernelConfig, KernelDevice};
    use crate::devices::{Device, DeviceConfig, DeviceStatus, DeviceType};
    use std::time::Duration;

    #[test]
    fn test_kernel_config_default() {
        let config = KernelConfig::default();
        assert_eq!(config.ip_address, "127.0.0.1");
        assert_eq!(config.port, 6767);
        assert_eq!(config.buffer_size, 8192);
        assert_eq!(config.connection_timeout_ms, 5000);
        assert_eq!(config.max_reconnect_attempts, 10);
        assert_eq!(config.connection_health_check_interval_ms, 10000);
    }

    #[test]
    fn test_kernel_device_new() {
        let device = KernelDevice::new("192.168.1.50".to_string());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_kernel_device_info() {
        let device = KernelDevice::new("192.168.1.100".to_string());
        let info = device.get_info();

        assert!(info.id.starts_with("kernel_"));
        assert!(info.name.contains("Kernel Flow2"));
        assert_eq!(info.device_type, DeviceType::Kernel);
        assert_eq!(info.status, DeviceStatus::Disconnected);

        // Check metadata contains expected fields
        assert!(info.metadata.get("ip_address").is_some());
        assert!(info.metadata.get("port").is_some());
    }

    #[tokio::test]
    async fn test_kernel_device_status() {
        let device = KernelDevice::new("192.168.1.100".to_string());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_connect_to_invalid_address() {
        let mut device = KernelDevice::new("0.0.0.0".to_string());
        let result = tokio::time::timeout(Duration::from_secs(2), device.connect()).await;

        // Should timeout or fail
        assert!(result.is_err() || result.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_disconnect_when_not_connected() {
        let mut device = KernelDevice::new("192.168.1.100".to_string());
        let result = device.disconnect().await;

        // Should succeed even when not connected
        assert!(result.is_ok());
        assert_eq!(device.get_status(), DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_send_when_disconnected() {
        let mut device = KernelDevice::new("192.168.1.100".to_string());
        let result = device.send(b"TEST").await;

        // Should fail when not connected
        assert!(result.is_err());
    }

    #[test]
    fn test_device_config() {
        let mut device = KernelDevice::new("192.168.1.100".to_string());

        let config = DeviceConfig {
            auto_reconnect: true,
            reconnect_interval_ms: 2000,
            timeout_ms: 5000,
            custom_settings: serde_json::json!({
                "ip_address": "192.168.1.200",
                "port": 7777,
                "buffer_size": 16384
            }),
        };

        let result = device.configure(config.clone());
        assert!(result.is_ok());
    }

    #[test]
    fn test_debug_implementation() {
        let device = KernelDevice::new("192.168.1.100".to_string());
        let debug_str = format!("{:?}", device);

        // Verify debug output contains expected fields
        assert!(debug_str.contains("KernelDevice"));
        assert!(debug_str.contains("ip_address"));
        assert!(debug_str.contains("192.168.1.100"));
    }

    #[test]
    fn test_kernel_config_with_custom_values() {
        let config = KernelConfig {
            ip_address: "10.0.0.1".to_string(),
            port: 8888,
            buffer_size: 4096,
            connection_timeout_ms: 10000,
            connection_health_check_interval_ms: 3000,
            max_reconnect_attempts: 10,
            initial_reconnect_delay_ms: 100,
            max_reconnect_delay_ms: 5000,
        };

        assert_eq!(config.ip_address, "10.0.0.1");
        assert_eq!(config.port, 8888);
        assert_eq!(config.buffer_size, 4096);
        assert_eq!(config.connection_health_check_interval_ms, 3000);
    }
}

#[cfg(test)]
mod protocol_tests {
    use crate::devices::kernel::KernelEvent;

    /// Test that KernelEvent with specific timestamp works
    #[test]
    fn test_kernel_event_with_timestamp() {
        let timestamp = 1234567890_i64;
        let event =
            KernelEvent::with_timestamp(42, timestamp, "marker", serde_json::json!({"key": "val"}));

        assert_eq!(event.id, 42);
        assert_eq!(event.timestamp, timestamp);
        assert_eq!(event.event, "marker");
        assert_eq!(event.value, serde_json::json!({"key": "val"}));
    }

    /// Test the length-prefixed wire format per Kernel Tasks SDK specification
    #[test]
    fn test_kernel_event_wire_format_structure() {
        let event = KernelEvent::with_timestamp(1, 1000000, "test", serde_json::json!("value"));

        let wire_data = event.to_wire_format().expect("Wire format should succeed");

        // Wire format should be: [4-byte length prefix][JSON payload]
        assert!(wire_data.len() > 4, "Wire data should have length prefix");

        // Extract the length prefix (first 4 bytes, big-endian u32)
        let length_bytes: [u8; 4] = wire_data[0..4].try_into().unwrap();
        let declared_length = u32::from_be_bytes(length_bytes);

        // The declared length should match the actual JSON payload length
        let json_payload = &wire_data[4..];
        assert_eq!(
            declared_length as usize,
            json_payload.len(),
            "Length prefix should match JSON payload size"
        );
    }

    /// Test that the JSON payload contains required schema fields
    #[test]
    fn test_kernel_event_json_schema() {
        let event =
            KernelEvent::with_timestamp(123, 9876543210, "stimulus", serde_json::json!("onset"));

        let wire_data = event.to_wire_format().expect("Wire format should succeed");
        let json_payload = &wire_data[4..];

        // Parse the JSON payload
        let parsed: serde_json::Value =
            serde_json::from_slice(json_payload).expect("JSON should be valid");

        // Verify required fields per Kernel Tasks SDK spec
        assert_eq!(parsed["id"], 123, "id field should match");
        assert_eq!(
            parsed["timestamp"], 9876543210_i64,
            "timestamp should match"
        );
        assert_eq!(parsed["event"], "stimulus", "event field should match");
        assert_eq!(parsed["value"], "onset", "value field should match");
    }

    /// Test wire format with complex value object
    #[test]
    fn test_kernel_event_complex_value() {
        let complex_value = serde_json::json!({
            "trial_number": 5,
            "condition": "control",
            "response_time_ms": 342.5,
            "correct": true
        });

        let event = KernelEvent::with_timestamp(99, 1000, "response", complex_value.clone());
        let wire_data = event.to_wire_format().expect("Wire format should succeed");

        // Extract and verify
        let length_bytes: [u8; 4] = wire_data[0..4].try_into().unwrap();
        let declared_length = u32::from_be_bytes(length_bytes);
        let json_payload = &wire_data[4..];

        assert_eq!(declared_length as usize, json_payload.len());

        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();
        assert_eq!(parsed["value"], complex_value);
    }

    /// Test that empty events work correctly
    #[test]
    fn test_kernel_event_empty_string_value() {
        let event = KernelEvent::with_timestamp(1, 0, "empty", serde_json::json!(""));

        let wire_data = event.to_wire_format().expect("Wire format should succeed");
        assert!(wire_data.len() > 4);

        let json_payload = &wire_data[4..];
        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();
        assert_eq!(parsed["value"], "");
    }

    /// Test wire format byte order is big-endian as specified
    #[test]
    fn test_kernel_event_big_endian_length() {
        // Create an event that produces a known-length JSON
        let event = KernelEvent::with_timestamp(1, 1, "x", serde_json::json!("y"));

        let wire_data = event.to_wire_format().expect("Wire format should succeed");

        // Verify we can correctly decode the length as big-endian
        let length_be =
            u32::from_be_bytes([wire_data[0], wire_data[1], wire_data[2], wire_data[3]]);
        let _length_le =
            u32::from_le_bytes([wire_data[0], wire_data[1], wire_data[2], wire_data[3]]);

        // The actual JSON length
        let actual_json_len = wire_data.len() - 4;

        assert_eq!(
            length_be as usize, actual_json_len,
            "Big-endian interpretation should match"
        );
        // For small JSON payloads, LE and BE might coincidentally match, but let's verify the format
        // by checking that our documented format (BE) is what we're using
        assert_eq!(
            length_be as usize, actual_json_len,
            "Length should be readable as big-endian"
        );
    }
}

#[cfg(test)]
mod event_id_tests {
    use crate::devices::kernel::KernelDevice;
    use crate::devices::Device;

    /// A new device reports that its first auto-assigned event id will be 1
    #[tokio::test]
    async fn test_event_ids_start_at_one() {
        let device = KernelDevice::new("192.168.1.100".to_string());

        // Verify device starts with event_id = 1
        let info = device.get_info();
        assert_eq!(info.metadata["next_event_id"], 1);
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::devices::kernel::KernelDevice;
    use crate::devices::{Device, DeviceType};
    use std::time::Duration;
    use tokio::time::timeout;

    const TEST_IP: &str = "192.168.1.100";

    #[tokio::test]
    #[ignore] // Ignore by default since it requires hardware
    async fn test_real_device_connection() {
        let mut device = KernelDevice::new(TEST_IP.to_string());

        // Attempt to connect with timeout
        let connect_result = timeout(Duration::from_secs(5), device.connect()).await;

        if let Ok(Ok(())) = connect_result {
            // If connected, test sending data
            let test_data = b"TEST_DATA";
            let send_result = device.send(test_data).await;
            assert!(send_result.is_ok());

            // Disconnect
            let disconnect_result = device.disconnect().await;
            assert!(disconnect_result.is_ok());
        } else {
            // Hardware not available, skip test
            println!("Kernel Flow2 hardware not available, skipping integration test");
        }
    }

    #[tokio::test]
    async fn test_concurrent_kernel_devices() {
        use tokio::task;

        // Test that multiple Kernel devices can be created
        let handles: Vec<_> = (0..3)
            .map(|i| {
                task::spawn(async move {
                    let device = KernelDevice::new(format!("192.168.1.{}", 100 + i));
                    device.get_info()
                })
            })
            .collect();

        for handle in handles {
            let info = handle.await.unwrap();
            assert!(info.id.starts_with("kernel_"));
            assert_eq!(info.device_type, DeviceType::Kernel);
        }
    }

    #[tokio::test]
    async fn test_buffer_overflow_handling() {
        let mut device = KernelDevice::new(TEST_IP.to_string());

        // Create large data that exceeds buffer
        let large_data = vec![0u8; 16384];

        // Without connection, this should fail gracefully
        let result = device.send(&large_data).await;
        assert!(result.is_err());
    }
}

/// Tests using a mock TCP server to verify wire protocol
#[cfg(test)]
mod mock_server_tests {
    use crate::devices::kernel::KernelDevice;
    use crate::devices::Device;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;
    use tokio::time::timeout;

    /// Test that connecting to a mock server works
    #[tokio::test]
    async fn test_connect_to_mock_server() {
        // Start a mock TCP server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn server task
        let server_handle = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            // Just accept the connection, don't do anything
        });

        // Connect device to mock server
        let mut device = KernelDevice::new(addr.ip().to_string());
        // Update port to match the dynamically assigned one
        device.update_kernel_config(crate::devices::kernel::KernelConfig {
            ip_address: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        });

        let connect_result = timeout(Duration::from_secs(2), device.connect()).await;
        assert!(connect_result.is_ok());
        assert!(connect_result.unwrap().is_ok());

        // Cleanup
        device.disconnect().await.unwrap();
        server_handle.abort();
    }

    /// Test that send_event transmits correct wire format to server
    #[tokio::test]
    async fn test_send_event_wire_format_to_server() {
        // Shared buffer to capture what the server receives
        let received_data = Arc::new(Mutex::new(Vec::new()));
        let received_data_clone = received_data.clone();

        // Start mock server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 1024];

            // Read data from client
            if let Ok(n) = socket.read(&mut buffer).await {
                let mut data = received_data_clone.lock().await;
                data.extend_from_slice(&buffer[..n]);
            }
        });

        // Connect and send event
        let mut device = KernelDevice::new(addr.ip().to_string());
        device.update_kernel_config(crate::devices::kernel::KernelConfig {
            ip_address: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        });

        device.connect().await.unwrap();

        // Send a test event (the device assigns the first id, 1)
        device
            .send_event(serde_json::json!({ "event": "test_marker", "value": "test_value" }))
            .await
            .unwrap();

        // Give server time to receive
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify received data
        let data = received_data.lock().await;
        assert!(data.len() > 4, "Should receive length prefix + JSON");

        // Parse length prefix
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(
            length as usize,
            data.len() - 4,
            "Length prefix should match payload"
        );

        // Parse JSON payload
        let json_payload = &data[4..];
        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();

        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["event"], "test_marker");
        assert_eq!(parsed["value"], "test_value");
        assert!(parsed["timestamp"].as_i64().unwrap() > 0);

        // Cleanup
        device.disconnect().await.unwrap();
        server_handle.abort();
    }

    /// Test that send_event injects id when the incoming JSON is missing it
    #[tokio::test]
    async fn test_send_event_injects_id_when_missing() {
        let received_data = Arc::new(Mutex::new(Vec::new()));
        let received_data_clone = received_data.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 1024];
            if let Ok(n) = socket.read(&mut buffer).await {
                let mut data = received_data_clone.lock().await;
                data.extend_from_slice(&buffer[..n]);
            }
        });

        let mut device = KernelDevice::new(addr.ip().to_string());
        device.update_kernel_config(crate::devices::kernel::KernelConfig {
            ip_address: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        });
        device.connect().await.unwrap();

        // Send event WITHOUT id field (simulates what the frontend sends)
        let event_without_id = serde_json::json!({
            "timestamp": 1700000000000000_i64,
            "event": "start_experiment",
            "value": "1"
        });
        device.send_event(event_without_id).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let data = received_data.lock().await;
        assert!(data.len() > 4, "Should receive length prefix + JSON");

        let json_payload = &data[4..];
        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();

        // id should have been auto-injected as 1
        assert_eq!(
            parsed["id"], 1,
            "send_event should inject id=1 when missing"
        );
        assert_eq!(parsed["timestamp"], 1700000000000000_i64);
        assert_eq!(parsed["event"], "start_experiment");
        assert_eq!(parsed["value"], "1");

        device.disconnect().await.unwrap();
        server_handle.abort();
    }

    /// Test that send_event preserves id when already present in the JSON
    #[tokio::test]
    async fn test_send_event_preserves_existing_id() {
        let received_data = Arc::new(Mutex::new(Vec::new()));
        let received_data_clone = received_data.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 1024];
            if let Ok(n) = socket.read(&mut buffer).await {
                let mut data = received_data_clone.lock().await;
                data.extend_from_slice(&buffer[..n]);
            }
        });

        let mut device = KernelDevice::new(addr.ip().to_string());
        device.update_kernel_config(crate::devices::kernel::KernelConfig {
            ip_address: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        });
        device.connect().await.unwrap();

        // Send event WITH id field already present
        let event_with_id = serde_json::json!({
            "id": 42,
            "timestamp": 1700000000000000_i64,
            "event": "end_experiment",
            "value": "1"
        });
        device.send_event(event_with_id).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let data = received_data.lock().await;
        assert!(data.len() > 4);

        let json_payload = &data[4..];
        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();

        // id should be preserved as 42
        assert_eq!(parsed["id"], 42, "send_event should preserve existing id");
        assert_eq!(parsed["event"], "end_experiment");

        device.disconnect().await.unwrap();
        server_handle.abort();
    }

    /// Test that send_event auto-assigns incrementing ids for multiple events without id
    #[tokio::test]
    async fn test_send_event_increments_auto_id() {
        let received_data = Arc::new(Mutex::new(Vec::new()));
        let received_data_clone = received_data.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut data = received_data_clone.lock().await;
                        data.extend_from_slice(&buffer[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        let mut device = KernelDevice::new(addr.ip().to_string());
        device.update_kernel_config(crate::devices::kernel::KernelConfig {
            ip_address: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        });
        device.connect().await.unwrap();

        // Send two events without id
        for event_name in &["start_experiment", "event_marker"] {
            let event = serde_json::json!({
                "timestamp": 1700000000000000_i64,
                "event": event_name,
                "value": "1"
            });
            device.send_event(event).await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Parse the two events from the received data
        let data = received_data.lock().await;
        let mut offset = 0;
        let mut ids = Vec::new();

        while offset + 4 < data.len() {
            let length = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            offset += 4;
            if offset + length > data.len() {
                break;
            }
            let parsed: serde_json::Value =
                serde_json::from_slice(&data[offset..offset + length]).unwrap();
            ids.push(parsed["id"].as_i64().unwrap());
            offset += length;
        }

        assert_eq!(ids, vec![1, 2], "Auto-assigned ids should increment: 1, 2");

        device.disconnect().await.unwrap();
        server_handle.abort();
    }
}
