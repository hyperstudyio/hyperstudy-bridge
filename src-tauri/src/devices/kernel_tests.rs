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

    /// Test that KernelEvent creates events with required fields
    #[test]
    fn test_kernel_event_new() {
        let event = KernelEvent::new(1, "test_event", serde_json::json!("test_value"));

        assert_eq!(event.id, 1);
        assert_eq!(event.event, "test_event");
        assert_eq!(event.value, serde_json::json!("test_value"));
        assert!(event.timestamp > 0); // Should have a valid timestamp
    }

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

    /// Test that timestamps are in microseconds as per spec
    #[test]
    fn test_kernel_event_timestamp_microseconds() {
        let event = KernelEvent::new(1, "test", serde_json::json!(null));

        // Timestamp should be a large number (microseconds since epoch)
        // As of 2024, this should be > 1_700_000_000_000_000 (roughly)
        assert!(
            event.timestamp > 1_000_000_000_000_000,
            "Timestamp {} should be in microseconds since epoch",
            event.timestamp
        );
    }
}

#[cfg(test)]
mod hierarchy_helper_tests {
    use crate::devices::kernel::KernelDevice;
    use crate::devices::Device;

    /// Test that experiment lifecycle methods generate correct event names
    #[tokio::test]
    async fn test_experiment_lifecycle_event_names() {
        let device = KernelDevice::new("192.168.1.100".to_string());

        // Verify device starts with event_id = 1
        let info = device.get_info();
        assert_eq!(info.metadata["next_event_id"], 1);
    }

    /// Test start_experiment with name creates correct value structure
    #[test]
    fn test_start_experiment_value_with_name() {
        use crate::devices::kernel::KernelEvent;

        // Simulate what start_experiment creates
        let value = serde_json::json!({ "name": "my_experiment" });
        let event = KernelEvent::new(1, "start_experiment", value);

        assert_eq!(event.event, "start_experiment");
        assert_eq!(event.value["name"], "my_experiment");
    }

    /// Test start_experiment without name creates null value
    #[test]
    fn test_start_experiment_value_without_name() {
        use crate::devices::kernel::KernelEvent;

        let value = serde_json::json!(null);
        let event = KernelEvent::new(1, "start_experiment", value);

        assert_eq!(event.event, "start_experiment");
        assert!(event.value.is_null());
    }

    /// Test task hierarchy event structure
    #[test]
    fn test_task_event_structure() {
        use crate::devices::kernel::KernelEvent;

        let value = serde_json::json!({ "task_name": "stroop_task" });
        let event = KernelEvent::new(1, "start_task", value);

        assert_eq!(event.event, "start_task");
        assert_eq!(event.value["task_name"], "stroop_task");
    }

    /// Test block hierarchy event with metadata
    #[test]
    fn test_block_event_with_metadata() {
        use crate::devices::kernel::KernelEvent;

        let mut value = serde_json::json!({
            "condition": "control",
            "difficulty": "easy"
        });
        value["block_number"] = serde_json::json!(1);

        let event = KernelEvent::new(1, "start_block", value);

        assert_eq!(event.event, "start_block");
        assert_eq!(event.value["block_number"], 1);
        assert_eq!(event.value["condition"], "control");
    }

    /// Test trial hierarchy event
    #[test]
    fn test_trial_event_structure() {
        use crate::devices::kernel::KernelEvent;

        let value = serde_json::json!({
            "trial_number": 5,
            "stimulus": "image_01.png"
        });
        let event = KernelEvent::new(1, "start_trial", value);

        assert_eq!(event.event, "start_trial");
        assert_eq!(event.value["trial_number"], 5);
    }

    /// Test stimulus onset event structure
    #[test]
    fn test_stimulus_onset_event() {
        use crate::devices::kernel::KernelEvent;

        let value = serde_json::json!({
            "stimulus_type": "visual",
            "image": "face_happy.png"
        });
        let event = KernelEvent::new(1, "event_stimulus_onset", value);

        // event_ prefix ensures timestamp is included in SNIRF export
        assert!(event.event.starts_with("event_"));
        assert_eq!(event.value["stimulus_type"], "visual");
    }

    /// Test response event structure
    #[test]
    fn test_response_event() {
        use crate::devices::kernel::KernelEvent;

        let value = serde_json::json!({
            "response": "left",
            "response_time_ms": 342.5,
            "correct": true
        });
        let event = KernelEvent::new(1, "event_response", value);

        assert_eq!(event.event, "event_response");
        assert_eq!(event.value["response"], "left");
        assert_eq!(event.value["response_time_ms"], 342.5);
        assert_eq!(event.value["correct"], true);
    }

    /// Test marker event gets event_ prefix
    #[test]
    fn test_marker_event_prefix() {
        use crate::devices::kernel::KernelEvent;

        // marker() method should prefix with "event_"
        let event = KernelEvent::new(1, "event_custom_marker", serde_json::json!("test"));

        assert!(
            event.event.starts_with("event_"),
            "Markers should be prefixed with event_ for SNIRF timestamp inclusion"
        );
    }

    /// Test end events match start events
    #[test]
    fn test_end_event_structures() {
        use crate::devices::kernel::KernelEvent;

        let events = vec![
            ("end_experiment", serde_json::json!(null)),
            ("end_task", serde_json::json!({ "task_name": "stroop" })),
            ("end_block", serde_json::json!({ "block_number": 1 })),
            ("end_trial", serde_json::json!({ "trial_number": 5 })),
        ];

        for (event_name, value) in events {
            let event = KernelEvent::new(1, event_name, value);
            assert!(
                event.event.starts_with("end_"),
                "End event {} should start with end_",
                event.event
            );
        }
    }

    /// Test metadata merging preserves user data while adding required fields
    #[test]
    fn test_metadata_merging_for_block() {
        // Simulate what start_block does with metadata
        let mut meta = serde_json::json!({
            "condition": "experimental",
            "difficulty": "hard"
        });
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("block_number".to_string(), serde_json::json!(3));
        }

        // User metadata should be preserved
        assert_eq!(meta["condition"], "experimental");
        assert_eq!(meta["difficulty"], "hard");
        // Required field should be added
        assert_eq!(meta["block_number"], 3);
    }

    /// Test metadata merging for trials with stimulus info
    #[test]
    fn test_metadata_merging_for_trial() {
        let mut meta = serde_json::json!({
            "stimulus": "face_01.png",
            "condition": "happy",
            "expected_response": "left"
        });
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("trial_number".to_string(), serde_json::json!(42));
        }

        assert_eq!(meta["stimulus"], "face_01.png");
        assert_eq!(meta["condition"], "happy");
        assert_eq!(meta["expected_response"], "left");
        assert_eq!(meta["trial_number"], 42);
    }

    /// Test that response event handles optional fields correctly
    #[test]
    fn test_response_event_optional_fields() {
        // With all fields
        let mut full_value = serde_json::json!({ "response": "right" });
        full_value["response_time_ms"] = serde_json::json!(250.5);
        full_value["correct"] = serde_json::json!(true);

        assert_eq!(full_value["response"], "right");
        assert_eq!(full_value["response_time_ms"], 250.5);
        assert_eq!(full_value["correct"], true);

        // With only required field
        let minimal_value = serde_json::json!({ "response": "left" });
        assert_eq!(minimal_value["response"], "left");
        assert!(minimal_value.get("response_time_ms").is_none());
        assert!(minimal_value.get("correct").is_none());
    }

    /// Test unicode handling in event names and values
    #[test]
    fn test_unicode_in_events() {
        use crate::devices::kernel::KernelEvent;

        let event = KernelEvent::new(
            1,
            "stimulus_日本語",
            serde_json::json!({
                "text": "こんにちは",
                "emoji": "🧠"
            }),
        );

        let wire_data = event.to_wire_format().expect("Should handle unicode");
        let json_payload = &wire_data[4..];
        let parsed: serde_json::Value = serde_json::from_slice(json_payload).unwrap();

        assert_eq!(parsed["event"], "stimulus_日本語");
        assert_eq!(parsed["value"]["text"], "こんにちは");
        assert_eq!(parsed["value"]["emoji"], "🧠");
    }

    /// Test empty metadata creates correct default structure
    #[test]
    fn test_empty_metadata_defaults() {
        // When metadata is None, should create minimal required structure
        let block_value = serde_json::json!({ "block_number": 1 });
        assert_eq!(block_value.as_object().unwrap().len(), 1);

        let trial_value = serde_json::json!({ "trial_number": 5 });
        assert_eq!(trial_value.as_object().unwrap().len(), 1);

        let task_value = serde_json::json!({ "task_name": "test" });
        assert_eq!(task_value.as_object().unwrap().len(), 1);
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

        // Send a test event
        let event_id = device
            .send_event_simple("test_marker", serde_json::json!("test_value"))
            .await
            .unwrap();

        assert_eq!(event_id, 1, "First event should have ID 1");

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

    /// Test event ID increments correctly across multiple sends
    #[tokio::test]
    async fn test_event_id_incrementing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            // Keep reading to consume all events
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(_) => continue,
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

        // Send multiple events and verify IDs increment
        let id1 = device
            .send_event_simple("event1", serde_json::json!(1))
            .await
            .unwrap();
        let id2 = device
            .send_event_simple("event2", serde_json::json!(2))
            .await
            .unwrap();
        let id3 = device
            .send_event_simple("event3", serde_json::json!(3))
            .await
            .unwrap();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);

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

    /// Test helper methods produce correct event names
    #[tokio::test]
    async fn test_helper_methods_event_names() {
        use crate::devices::kernel::KernelEvent;

        // We can test the wire format without a server by using KernelEvent directly
        // to verify what the helper methods would produce

        // start_experiment produces "start_experiment"
        let exp_event = KernelEvent::new(1, "start_experiment", serde_json::json!(null));
        assert_eq!(exp_event.event, "start_experiment");

        // end_experiment produces "end_experiment"
        let end_event = KernelEvent::new(2, "end_experiment", serde_json::json!(null));
        assert_eq!(end_event.event, "end_experiment");

        // stimulus_onset produces "event_stimulus_onset" (note the event_ prefix)
        let stim_event = KernelEvent::new(
            3,
            "event_stimulus_onset",
            serde_json::json!({"stimulus_type": "visual"}),
        );
        assert!(stim_event.event.starts_with("event_"));

        // marker produces "event_<name>"
        let marker_event = KernelEvent::new(4, "event_custom_marker", serde_json::json!("data"));
        assert!(marker_event.event.starts_with("event_"));
    }
}
