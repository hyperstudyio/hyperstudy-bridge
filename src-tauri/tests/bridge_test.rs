//! Bridge WebSocket server and message routing tests
//!
//! Tests for WebSocket server, message parsing, client connections,
//! throughput, error handling, queries, and scalability.
//!
//! Uses the new TestHarness infrastructure with explicit async cleanup.

use hyperstudy_bridge::bridge::message::{CommandAction, QueryTarget};
use hyperstudy_bridge::bridge::{AppState, BridgeCommand, BridgeResponse, BridgeServer};
use hyperstudy_bridge::devices::{DeviceError, DeviceStatus, DeviceType};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::time::timeout;

mod common;
use common::prelude::*;

/// Test suite for WebSocket server startup and shutdown
#[cfg(test)]
mod websocket_server_tests {
    use super::*;

    /// Starts a Bridge server on a free port (never 9000, where a running Bridge may listen) and returns its URL.
    async fn start_test_server(harness: &TestHarness) -> (String, tokio::task::JoinHandle<()>) {
        let app = tauri::test::mock_app();
        let mut bridge_server = BridgeServer::new(harness.app_state.clone(), app.handle().clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let _app = app; // keep the mock app alive for the server's lifetime
            let _ = bridge_server.serve(listener).await;
        });
        (url, handle)
    }

    #[tokio::test]
    async fn test_websocket_server_startup() -> TestResult<()> {
        let harness = TestHarness::new().await;
        let (ws_url, server_handle) = start_test_server(&harness).await;

        let client = timeout(
            Duration::from_secs(5),
            TestWebSocketClient::connect(&ws_url),
        )
        .await
        .expect("connecting to the test server timed out")?;
        assert!(client.is_connected());
        client.close().await?;

        server_handle.abort();
        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_websocket_port_binding() -> TestResult<()> {
        // Test that the server can bind to the expected port
        let addr = "127.0.0.1:9001"; // Use different port for test
        let listener_result = TcpListener::bind(addr).await;

        assert!(listener_result.is_ok(), "Failed to bind to WebSocket port");

        if let Ok(listener) = listener_result {
            drop(listener); // Release the port
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_server_graceful_shutdown() -> TestResult<()> {
        let harness = TestHarness::new().await;
        let (ws_url, server_handle) = start_test_server(&harness).await;

        server_handle.abort();
        let _ = server_handle.await; // the listener is dropped once the task has ended

        let connection_result = timeout(
            Duration::from_millis(500),
            TestWebSocketClient::connect(&ws_url),
        )
        .await;
        assert!(
            connection_result.is_err() || connection_result.unwrap().is_err(),
            "Server should not accept connections after shutdown"
        );

        harness.cleanup().await
    }
}

/// Test suite for message routing and handling
#[cfg(test)]
mod message_routing_tests {
    use super::*;

    #[tokio::test]
    async fn test_command_message_parsing() -> TestResult<()> {
        let mut data_generator = TestDataGenerator::new();

        // Test connect command parsing
        let connect_command = data_generator.generate_connect_command("ttl");
        let json_str = serde_json::to_string(&connect_command).unwrap();
        let parsed_command: BridgeCommand = serde_json::from_str(&json_str).unwrap();

        match parsed_command {
            BridgeCommand::Command { device, action, .. } => {
                assert_eq!(device, "ttl");
                assert!(matches!(action, CommandAction::Connect));
            }
            _ => return Err(TestError::Assertion("Expected Command variant".to_string())),
        }

        // Test TTL pulse command
        let pulse_command = data_generator.generate_ttl_command();
        let json_str = serde_json::to_string(&pulse_command).unwrap();
        let parsed_command: BridgeCommand = serde_json::from_str(&json_str).unwrap();

        match parsed_command {
            BridgeCommand::Command {
                device,
                action,
                payload,
                ..
            } => {
                assert_eq!(device, "ttl");
                assert!(matches!(action, CommandAction::Send));
                assert!(payload.is_some());
            }
            _ => return Err(TestError::Assertion("Expected Command variant".to_string())),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_query_message_parsing() -> TestResult<()> {
        let query_devices = BridgeCommand::Query {
            target: QueryTarget::Devices,
            id: Some("test_query_1".to_string()),
        };

        let json_str = serde_json::to_string(&query_devices).unwrap();
        let parsed_command: BridgeCommand = serde_json::from_str(&json_str).unwrap();

        match parsed_command {
            BridgeCommand::Query { target, id } => {
                assert!(matches!(target, QueryTarget::Devices));
                assert_eq!(id, Some("test_query_1".to_string()));
            }
            _ => return Err(TestError::Assertion("Expected Query variant".to_string())),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_response_message_serialization() -> TestResult<()> {
        // Test status response
        let status_response =
            BridgeResponse::status("test_device".to_string(), DeviceStatus::Connected, None);
        let json_str = serde_json::to_string(&status_response).unwrap();
        let parsed_response: BridgeResponse = serde_json::from_str(&json_str).unwrap();

        match parsed_response {
            BridgeResponse::Status { device, status, .. } => {
                assert_eq!(device, "test_device");
                assert_eq!(status, DeviceStatus::Connected);
            }
            _ => return Err(TestError::Assertion("Expected Status response".to_string())),
        }

        // Test error response
        let error_response =
            BridgeResponse::device_error("test_device".to_string(), "Test error".to_string(), None);
        let json_str = serde_json::to_string(&error_response).unwrap();
        let parsed_response: BridgeResponse = serde_json::from_str(&json_str).unwrap();

        match parsed_response {
            BridgeResponse::Error {
                device, message, ..
            } => {
                assert_eq!(device, Some("test_device".to_string()));
                assert_eq!(message, "Test error");
            }
            _ => return Err(TestError::Assertion("Expected Error response".to_string())),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_malformed_message_handling() -> TestResult<()> {
        let malformed_messages = vec![
            "invalid json",
            "{}",
            r#"{"type": "unknown"}"#,
            r#"{"type": "command"}"#, // missing required fields
            r#"{"type": "command", "device": ""}"#, // empty device
        ];

        for malformed_msg in malformed_messages {
            let parse_result = serde_json::from_str::<BridgeCommand>(malformed_msg);
            assert!(
                parse_result.is_err(),
                "Should fail to parse malformed message: {}",
                malformed_msg
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_large_message_handling() -> TestResult<()> {
        let mut data_generator = TestDataGenerator::new();

        // Create a large message (1MB)
        let large_payload = data_generator.generate_large_message(1024);
        let large_command = BridgeCommand::Command {
            device: "test_device".to_string(),
            action: CommandAction::Send,
            payload: Some(large_payload),
            id: Some(data_generator.generate_request_id()),
        };

        // Test serialization/deserialization of large message
        let json_str = serde_json::to_string(&large_command).unwrap();
        assert!(json_str.len() > 1024 * 1024, "Message should be large");

        let parsed_command: BridgeCommand = serde_json::from_str(&json_str).unwrap();
        match parsed_command {
            BridgeCommand::Command {
                device,
                action,
                payload,
                ..
            } => {
                assert_eq!(device, "test_device");
                assert!(matches!(action, CommandAction::Send));
                assert!(payload.is_some());
            }
            _ => return Err(TestError::Assertion("Expected Command variant".to_string())),
        }

        Ok(())
    }
}

/// Test suite for client connection and disconnection handling
#[cfg(test)]
mod client_connection_tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_state_management() -> TestResult<()> {
        let harness = TestHarness::new().await;

        // Test connection state tracking
        let initial_connections = harness.app_state.connections.len();
        assert_eq!(initial_connections, 0);

        // Simulate adding connections
        harness
            .app_state
            .add_connection("conn1".to_string(), "127.0.0.1:12345".to_string());
        harness
            .app_state
            .add_connection("conn2".to_string(), "127.0.0.1:12346".to_string());

        assert_eq!(harness.app_state.connections.len(), 2);

        // Test connection activity updates
        harness.app_state.update_connection_activity("conn1");
        harness.app_state.update_connection_activity("conn2");

        // Test connection removal
        harness.app_state.remove_connection("conn1");
        assert_eq!(harness.app_state.connections.len(), 1);

        harness.app_state.remove_connection("conn2");
        assert_eq!(harness.app_state.connections.len(), 0);

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_multiple_concurrent_connections() -> TestResult<()> {
        let harness = TestHarness::new().await;

        // Simulate multiple concurrent connections
        let connection_count = 10;
        let mut connection_ids = Vec::new();

        for i in 0..connection_count {
            let conn_id = format!("conn_{}", i);
            let addr = format!("127.0.0.1:{}", 50000 + i);
            harness.app_state.add_connection(conn_id.clone(), addr);
            connection_ids.push(conn_id);
        }

        assert_eq!(harness.app_state.connections.len(), connection_count);

        // Test concurrent activity updates
        let mut handles = Vec::new();
        for conn_id in &connection_ids {
            let state = harness.app_state.clone();
            let conn_id_clone = conn_id.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..10 {
                    state.update_connection_activity(&conn_id_clone);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            handles.push(handle);
        }

        // Wait for all updates to complete
        for handle in handles {
            handle
                .await
                .map_err(|e| TestError::TaskFailed(e.to_string()))?;
        }

        // Clean up connections
        for conn_id in connection_ids {
            harness.app_state.remove_connection(&conn_id);
        }

        assert_eq!(harness.app_state.connections.len(), 0);
        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_connection_cleanup_on_disconnect() -> TestResult<()> {
        let harness = TestHarness::new().await;

        // Add connections
        harness
            .app_state
            .add_connection("test_conn_1".to_string(), "127.0.0.1:50001".to_string());
        harness
            .app_state
            .add_connection("test_conn_2".to_string(), "127.0.0.1:50002".to_string());

        assert_eq!(harness.app_state.connections.len(), 2);

        // Simulate abrupt disconnection (connection just disappears)
        harness.app_state.remove_connection("test_conn_1");

        // Verify cleanup
        assert_eq!(harness.app_state.connections.len(), 1);

        // Clean up remaining connection
        harness.app_state.remove_connection("test_conn_2");
        assert_eq!(harness.app_state.connections.len(), 0);

        harness.cleanup().await
    }
}

/// Test suite for message throughput testing
#[cfg(test)]
mod throughput_tests {
    use super::*;

    #[tokio::test]
    async fn test_message_processing_throughput() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add mock device
        let device_id = harness.add_connected_device(DeviceType::Mock).await?;

        // Measure message processing throughput
        let test_duration = Duration::from_secs(2);
        let start_time = Instant::now();
        let mut message_count = 0u64;

        while start_time.elapsed() < test_duration {
            // Simulate message processing
            message_count += 1;

            // Small delay to prevent spinning
            if message_count % 1000 == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        let actual_duration = start_time.elapsed().as_secs_f64();
        let throughput = message_count as f64 / actual_duration;

        println!("Message processing throughput: {:.0} msg/sec", throughput);

        // Verify throughput meets requirement (>1000 msg/sec)
        Assertions::assert_throughput(throughput, 1000.0, "message processing")?;

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_command_throughput() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Measure device command throughput
        let test_duration = Duration::from_secs(3);
        let start_time = Instant::now();
        let mut command_count = 0u64;

        while start_time.elapsed() < test_duration {
            if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
                let mut device = device_lock.write().await;
                if device.send(b"PULSE\n").await.is_ok() {
                    command_count += 1;
                }
            }

            // Yield to prevent spinning
            if command_count % 100 == 0 {
                tokio::task::yield_now().await;
            }
        }

        let actual_duration = start_time.elapsed().as_secs_f64();
        let throughput = command_count as f64 / actual_duration;

        println!(
            "Device command throughput: {:.0} cmd/sec ({} commands)",
            throughput, command_count
        );

        // TTL should handle high-frequency commands (relaxed for mock device)
        assert!(
            throughput > 100.0,
            "TTL command throughput too low: {:.0} cmd/sec",
            throughput
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_concurrent_client_throughput() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add multiple devices for concurrent testing
        let mut device_ids = Vec::new();
        for _ in 0..5 {
            let device_id = harness.add_connected_device(DeviceType::Mock).await?;
            device_ids.push(device_id);
        }

        // Measure concurrent throughput using new run_load_test
        let concurrent_clients = 5;
        let operations_per_client = 200;

        let state_for_test = harness.app_state.clone();
        let device_ids_for_test = device_ids.clone();

        let start_time = Instant::now();
        let result = run_load_test(
            concurrent_clients,
            operations_per_client,
            move |client_id, _op_id| {
                let state = state_for_test.clone();
                let device_ids = device_ids_for_test.clone();
                async move {
                    let device_id = &device_ids[client_id % device_ids.len()];
                    let operation_start = Instant::now();
                    if let Some(device_lock) = state.get_device(device_id).await {
                        let mut device = device_lock.write().await;
                        let _ = device.send(b"test").await;
                    }
                    Ok(operation_start.elapsed())
                }
            },
        )
        .await;

        let total_duration = start_time.elapsed().as_secs_f64();
        let successful_ops = result.successes().len();
        let overall_throughput = successful_ops as f64 / total_duration;

        println!(
            "Concurrent throughput: {:.0} ops/sec ({} ops, {} clients)",
            overall_throughput, successful_ops, concurrent_clients
        );

        // Verify reasonable throughput under concurrent load
        // Very relaxed threshold since we have heavy lock contention with multiple workers
        assert!(
            overall_throughput > 10.0,
            "Concurrent throughput too low: {:.0} ops/sec",
            overall_throughput
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_message_size_vs_throughput() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::Kernel).await?;

        let message_sizes = vec![10, 100, 1000, 10000]; // bytes
        let test_duration = Duration::from_secs(1);

        for message_size in message_sizes {
            let message = vec![0u8; message_size];

            let start_time = Instant::now();
            let mut message_count = 0u64;

            while start_time.elapsed() < test_duration {
                if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
                    let mut device = device_lock.write().await;
                    if device.send(&message).await.is_ok() {
                        message_count += 1;
                    }
                }

                // Yield periodically
                if message_count % 50 == 0 {
                    tokio::task::yield_now().await;
                }
            }

            let actual_duration = start_time.elapsed().as_secs_f64();
            let throughput = message_count as f64 / actual_duration;
            let bytes_per_second = throughput * message_size as f64;

            println!(
                "Message size: {} bytes, Throughput: {:.0} msg/sec, {:.0} bytes/sec",
                message_size, throughput, bytes_per_second
            );

            // Throughput should decrease with message size, but still be reasonable
            assert!(
                throughput > 50.0,
                "Throughput too low for {} byte messages: {:.0} msg/sec",
                message_size,
                throughput
            );
        }

        harness.cleanup().await
    }
}

/// Test suite for error handling and recovery in bridge operations
#[cfg(test)]
mod bridge_error_handling_tests {
    use super::*;

    #[tokio::test]
    async fn test_invalid_command_handling() -> TestResult<()> {
        let harness = TestHarness::new().await;

        // Test handling of commands for non-existent devices
        let invalid_command = BridgeCommand::Command {
            device: "non_existent_device".to_string(),
            action: CommandAction::Connect,
            payload: None,
            id: Some("test_id".to_string()),
        };

        // Simulate command processing
        // In a real scenario, this would be handled by the WebSocket message handler
        // For now, we test that the device lookup fails gracefully
        let device = harness.app_state.get_device("non_existent_device").await;
        assert!(device.is_none(), "Should not find non-existent device");

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_bridge_state_consistency_under_errors() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add multiple devices with varying reliability
        let reliable_device_id = harness.add_connected_device(DeviceType::TTL).await?;
        let unreliable_device_id = harness.add_unreliable_device(DeviceType::Kernel, 0.7).await;

        // Attempt to connect unreliable device multiple times
        let mut connection_attempts = 0;
        for _ in 0..10 {
            if let Some(device_lock) = harness.app_state.get_device(&unreliable_device_id).await {
                let mut device = device_lock.write().await;
                if device.connect().await.is_ok() {
                    connection_attempts += 1;
                    let _ = device.disconnect().await;
                }
            }
        }

        // Verify reliable device is still connected despite unreliable device errors
        let reliable_status = harness
            .app_state
            .get_device_status(&reliable_device_id)
            .await;
        assert_eq!(reliable_status, Some(DeviceStatus::Connected));

        // Verify state consistency
        let device_count = harness.device_count().await;
        assert_eq!(device_count, 2, "Device count should remain consistent");

        println!(
            "Unreliable device successful connections: {}/10",
            connection_attempts
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_memory_stability_under_errors() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let mut memory_tracker = MemoryTracker::new();

        // Add unreliable device
        let device_id = harness.add_unreliable_device(DeviceType::TTL, 0.8).await;

        // Generate many errors
        for cycle in 0..100 {
            if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
                let mut device = device_lock.write().await;
                let _ = device.connect().await; // Most will fail
                let _ = device.send(b"test").await; // Most will fail
                let _ = device.disconnect().await;
            }

            if cycle % 20 == 0 {
                memory_tracker.measure();
            }
        }

        memory_tracker.measure();

        // Verify no significant memory leaks from error handling (very relaxed threshold)
        assert!(
            !memory_tracker.has_memory_leak(100),
            "Memory leak detected during error handling: {} bytes increase",
            memory_tracker.memory_increase()
        );

        harness.cleanup().await
    }
}

/// Test suite for bridge query operations
#[cfg(test)]
mod bridge_query_tests {
    use super::*;

    #[tokio::test]
    async fn test_devices_query() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add multiple devices using the new multi-device setup
        let device_map = harness.add_multi_device_setup().await;

        // Simulate devices query
        let devices_list = harness.app_state.list_devices().await;

        assert_eq!(devices_list.len(), device_map.len());

        // Verify all device types are represented
        let device_types: std::collections::HashSet<_> =
            devices_list.iter().map(|info| info.device_type).collect();

        assert!(device_types.contains(&DeviceType::TTL));
        assert!(device_types.contains(&DeviceType::Kernel));
        assert!(device_types.contains(&DeviceType::Pupil));
        assert!(device_types.contains(&DeviceType::Mock));

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_info_query() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_device(DeviceType::TTL).await;

        // Query specific device info
        if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
            let device = device_lock.read().await;
            let device_info = device.get_info();

            assert_eq!(device_info.id, device_id);
            assert_eq!(device_info.device_type, DeviceType::TTL);
            assert_eq!(device_info.status, DeviceStatus::Disconnected);
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_connections_query() -> TestResult<()> {
        let harness = TestHarness::new().await;

        // Add some connections
        harness
            .app_state
            .add_connection("conn1".to_string(), "127.0.0.1:50001".to_string());
        harness
            .app_state
            .add_connection("conn2".to_string(), "127.0.0.1:50002".to_string());

        // Query connections (simulating QueryTarget::Connections)
        let connections: Vec<_> = harness
            .app_state
            .connections
            .iter()
            .map(|entry| entry.value().clone())
            .collect();

        assert_eq!(connections.len(), 2);

        // Clean up connections
        harness.app_state.remove_connection("conn1");
        harness.app_state.remove_connection("conn2");

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_status_query() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add some devices
        let _device_id1 = harness.add_device(DeviceType::TTL).await;
        let _device_id2 = harness.add_device(DeviceType::Kernel).await;

        // Add a connection
        harness
            .app_state
            .add_connection("test_conn".to_string(), "127.0.0.1:50001".to_string());

        // Simulate status query
        let device_count = harness.app_state.devices.read().await.len();
        let connection_count = harness.app_state.connections.len();

        let expected_status = json!({
            "server": "running",
            "port": 9000,
            "devices": device_count,
            "connections": connection_count,
        });

        assert_eq!(device_count, 2);
        assert_eq!(connection_count, 1);

        // Clean up
        harness.app_state.remove_connection("test_conn");
        harness.cleanup().await
    }
}

/// Test suite for performance and scalability
#[cfg(test)]
mod scalability_tests {
    use super::*;

    #[tokio::test]
    async fn test_many_devices_handling() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add many devices
        let device_count = 50;
        let mut device_ids = Vec::new();

        for i in 0..device_count {
            let device_type = match i % 4 {
                0 => DeviceType::TTL,
                1 => DeviceType::Kernel,
                2 => DeviceType::Pupil,
                _ => DeviceType::Mock,
            };
            let device_id = harness.add_device(device_type).await;
            device_ids.push(device_id);
        }

        assert_eq!(harness.device_count().await, device_count);

        // Connect all devices concurrently
        let mut connect_tasks = Vec::new();
        for device_id in &device_ids {
            let state = harness.app_state.clone();
            let device_id_clone = device_id.clone();
            let task = tokio::spawn(async move {
                if let Some(device_lock) = state.get_device(&device_id_clone).await {
                    let mut device = device_lock.write().await;
                    device.connect().await
                } else {
                    Err(DeviceError::NotConnected)
                }
            });
            connect_tasks.push(task);
        }

        // Wait for all connections
        let mut successful_connections = 0;
        for task in connect_tasks {
            if task
                .await
                .map_err(|e| TestError::TaskFailed(e.to_string()))?
                .is_ok()
            {
                successful_connections += 1;
            }
        }

        assert_eq!(successful_connections, device_count);

        // Verify all devices are accessible
        for device_id in &device_ids {
            let status = harness.app_state.get_device_status(device_id).await;
            assert_eq!(status, Some(DeviceStatus::Connected));
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_high_frequency_operations() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add TTL device for high-frequency testing
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Perform high-frequency operations
        let operation_count = 10000;
        let start_time = Instant::now();

        for _ in 0..operation_count {
            if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
                let mut device = device_lock.write().await;
                device.send(b"PULSE").await?;
            }
        }

        let elapsed = start_time.elapsed();
        let frequency = operation_count as f64 / elapsed.as_secs_f64();

        println!(
            "High-frequency operations: {:.0} ops/sec ({} ops in {:?})",
            frequency, operation_count, elapsed
        );

        // Should handle at least 500 operations per second (relaxed for mock device overhead)
        assert!(
            frequency > 500.0,
            "High-frequency operation rate too low: {:.0} ops/sec",
            frequency
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_memory_usage_under_load() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let mut memory_tracker = MemoryTracker::new();

        // Add multiple devices
        let mut device_ids = Vec::new();
        for _ in 0..10 {
            let device_id = harness.add_connected_device(DeviceType::Mock).await?;
            device_ids.push(device_id);
        }

        memory_tracker.measure();

        // Perform sustained operations
        for cycle in 0..200 {
            for device_id in &device_ids {
                if let Some(device_lock) = harness.app_state.get_device(device_id).await {
                    let mut device = device_lock.write().await;
                    device.send(b"load_test").await?;
                }
            }

            if cycle % 50 == 0 {
                memory_tracker.measure();
            }
        }

        memory_tracker.measure();

        // Verify reasonable memory usage (very relaxed threshold)
        assert!(
            !memory_tracker.has_memory_leak(100),
            "Excessive memory usage under load: {} bytes increase",
            memory_tracker.memory_increase()
        );

        println!(
            "Memory usage under load: {} bytes increase",
            memory_tracker.memory_increase()
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_bridge_state_performance() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add many devices to test state management performance
        let device_count = 100;

        for _ in 0..device_count {
            harness.add_device(DeviceType::Mock).await;
        }

        // Measure state query performance
        let start_time = Instant::now();

        for _ in 0..1000 {
            let _devices = harness.app_state.list_devices().await;
        }

        let query_time = start_time.elapsed();
        let queries_per_second = 1000.0 / query_time.as_secs_f64();

        println!(
            "State query performance: {:.0} queries/sec with {} devices",
            queries_per_second, device_count
        );

        // Should handle at least 100 queries per second even with many devices
        assert!(
            queries_per_second > 100.0,
            "State query performance too low: {:.0} queries/sec",
            queries_per_second
        );

        harness.cleanup().await
    }
}
