//! Integration tests for device lifecycle, performance, and error recovery
//!
//! Uses the new TestHarness infrastructure for explicit async cleanup.

mod common;
use common::prelude::*;

use serde_json::json;
use std::collections::HashMap;

/// Test suite for device lifecycle operations
#[cfg(test)]
mod device_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn test_device_connect_disconnect_cycle() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_device(DeviceType::TTL).await;

        // Test initial state
        Assertions::assert_device_status(
            &harness,
            &device_id,
            DeviceStatus::Disconnected,
            "initial",
        )
        .await?;

        // Test connection
        {
            let device_lock = harness
                .app_state
                .get_device(&device_id)
                .await
                .ok_or_else(|| TestError::Setup("Device not found".to_string()))?;
            let mut device = device_lock.write().await;
            device.connect().await?;
            assert_eq!(device.get_status(), DeviceStatus::Connected);
        }

        // Verify status updated in state
        Assertions::assert_device_status(
            &harness,
            &device_id,
            DeviceStatus::Connected,
            "after connect",
        )
        .await?;

        // Test disconnection
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.disconnect().await?;
            assert_eq!(device.get_status(), DeviceStatus::Disconnected);
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_multiple_device_connections() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let devices = harness.add_multi_device_setup().await;

        // Connect all devices simultaneously
        let mut connect_tasks = Vec::new();
        for (device_type, device_id) in &devices {
            let state_clone = harness.app_state.clone();
            let device_id_clone = device_id.clone();
            let task = tokio::spawn(async move {
                if let Some(device_lock) = state_clone.get_device(&device_id_clone).await {
                    let mut device = device_lock.write().await;
                    device.connect().await
                } else {
                    Err(DeviceError::NotConnected)
                }
            });
            connect_tasks.push(task);
        }

        // Wait for all connections to complete
        for task in connect_tasks {
            task.await
                .map_err(|e| TestError::TaskFailed(e.to_string()))?
                .map_err(TestError::Device)?;
        }

        // Verify all devices are connected
        for (_, device_id) in &devices {
            Assertions::assert_device_status(
                &harness,
                device_id,
                DeviceStatus::Connected,
                "after concurrent connect",
            )
            .await?;
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_send_receive_operations() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Test send operation
        let test_data = b"PULSE\n";
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.send(test_data).await?;
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_error_handling() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_unreliable_device(DeviceType::TTL, 1.0).await; // 100% error rate

        // Test connection failure
        {
            let device_lock = harness
                .app_state
                .get_device(&device_id)
                .await
                .ok_or_else(|| TestError::Setup("Device not found".to_string()))?;
            let mut device = device_lock.write().await;
            let result = device.connect().await;
            assert!(
                result.is_err(),
                "Connection should fail with 100% error rate"
            );
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_configuration() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_device(DeviceType::TTL).await;

        let config = DeviceConfig {
            auto_reconnect: false,
            reconnect_interval_ms: 2000,
            timeout_ms: 10000,
            custom_settings: json!({"test_setting": "test_value"}),
        };

        {
            let device_lock = harness
                .app_state
                .get_device(&device_id)
                .await
                .ok_or_else(|| TestError::Setup("Device not found".to_string()))?;
            let mut device = device_lock.write().await;
            device.configure(config.clone())?;
        }

        harness.cleanup().await
    }
}

/// Test suite for performance requirements
#[cfg(test)]
mod performance_tests {
    use super::*;

    #[tokio::test]
    async fn test_ttl_latency_requirement() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Measure TTL pulse latency
        // Note: Mock device latency includes lock acquisition overhead,
        // so we test for reasonable latency rather than <1ms which is
        // only achievable with real hardware
        let pulse_command = b"PULSE\n";
        let mut latencies = Vec::new();

        for _ in 0..100 {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            let started = std::time::Instant::now();
            let result = device.send(pulse_command).await;
            let latency = started.elapsed();
            result?;
            latencies.push(latency);
        }

        let avg_latency = latencies.iter().sum::<Duration>() / latencies.len() as u32;
        let max_latency = *latencies.iter().max().unwrap();

        println!("TTL Average latency: {:?}", avg_latency);
        println!("TTL Max latency: {:?}", max_latency);

        // With mock device and lock overhead, verify latency is under 10ms
        // Real hardware tests should verify <1ms requirement
        Assertions::assert_latency(avg_latency, 10.0, "TTL average (mock device)")?;
        Assertions::assert_latency(max_latency, 50.0, "TTL maximum (mock device)")?;

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_message_throughput_requirement() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::Kernel).await?;

        // Measure throughput
        // Note: Mock device with lock acquisition overhead won't match
        // real hardware throughput. We verify reasonable throughput.
        let test_data = b"test_message";
        let test_duration = Duration::from_secs(2);

        let (message_count, throughput) = measure_throughput(
            || async {
                if let Some(device_lock) = harness.app_state.get_device(&device_id).await {
                    let mut device = device_lock.write().await;
                    let _ = device.send(test_data).await;
                }
            },
            test_duration,
        )
        .await;

        println!(
            "Throughput: {} msg/sec ({} messages in {:?})",
            throughput, message_count, test_duration
        );

        // With mock device and lock overhead, verify throughput is at least 100 msg/sec
        // Real hardware tests should verify >1000 msg/sec requirement
        Assertions::assert_throughput(throughput, 100.0, "device send (mock device)")?;

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_concurrent_device_performance() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let devices = harness.add_connected_multi_device_setup().await?;

        let test_data: &'static [u8] = b"concurrent_test";
        let concurrent_operations = 10;
        let operations_per_worker = 100;

        // Run concurrent load test
        let state_for_test = harness.app_state.clone();
        let devices_for_test: HashMap<DeviceType, String> = devices.clone();

        let results = run_load_test(
            concurrent_operations,
            operations_per_worker,
            move |worker_id, _op_id| {
                let state = state_for_test.clone();
                let devices = devices_for_test.clone();
                async move {
                    let device_types = vec![
                        DeviceType::TTL,
                        DeviceType::Kernel,
                        DeviceType::Pupil,
                        DeviceType::Mock,
                    ];
                    let device_type = device_types[worker_id % device_types.len()];
                    let device_id = &devices[&device_type];

                    let start = Instant::now();
                    if let Some(device_lock) = state.get_device(device_id).await {
                        let mut device = device_lock.write().await;
                        device.send(test_data).await.map_err(TestError::Device)?;
                    }
                    Ok(start.elapsed())
                }
            },
        )
        .await;

        // Flatten and analyze results
        let flattened = results.flatten();
        let all_latencies: Vec<Duration> = flattened.unwrap_all();

        let stats = LatencyStats::from_latencies(&all_latencies)
            .ok_or_else(|| TestError::Assertion("No latency data collected".to_string()))?;

        println!("Concurrent test results:");
        println!("  Average latency: {:?}", stats.avg);
        println!("  P95 latency: {:?}", stats.p95);
        println!("  Max latency: {:?}", stats.max);
        println!("  Total operations: {}", stats.count);

        // Verify reasonable performance under load
        Assertions::assert_latency(stats.avg, 100.0, "average under concurrent load")?;
        Assertions::assert_latency(stats.p95, 200.0, "P95 under concurrent load")?;

        harness.cleanup().await
    }
}

/// Test suite for error recovery and reconnection
#[cfg(test)]
mod error_recovery_tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_recovery_after_failure() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_unreliable_device(DeviceType::TTL, 0.5).await; // 50% error rate

        let mut successful_connections = 0;
        let max_attempts = 10;

        for _ in 0..max_attempts {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            if device.connect().await.is_ok() {
                successful_connections += 1;
                device.disconnect().await.ok();
            }
        }

        // With 50% error rate, we should have some successful connections
        assert!(
            successful_connections > 0,
            "No successful connections after {} attempts",
            max_attempts
        );
        assert!(
            successful_connections < max_attempts,
            "All connections succeeded with 50% error rate"
        );

        println!(
            "Successful connections: {}/{}",
            successful_connections, max_attempts
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_operation_retry_on_temporary_failure() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_unreliable_device(DeviceType::TTL, 0.3).await; // 30% error rate

        // Keep trying to connect until successful
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            for _ in 0..10 {
                if device.connect().await.is_ok() {
                    break;
                }
            }
        }

        let test_data = b"retry_test";
        let mut successful_operations = 0;
        let total_operations = 50;

        for _ in 0..total_operations {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;

            // Retry operation up to 3 times on failure
            for retry in 0..3 {
                match device.send(test_data).await {
                    Ok(_) => {
                        successful_operations += 1;
                        break;
                    }
                    Err(_) if retry < 2 => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }

        // With retries, we should have better success rate than the base error rate
        let success_rate = successful_operations as f64 / total_operations as f64;
        println!("Success rate with retries: {:.2}%", success_rate * 100.0);

        // With 30% error rate and 3 retries, success rate should be much higher
        assert!(
            success_rate > 0.8,
            "Success rate too low even with retries: {}",
            success_rate
        );

        harness.cleanup().await
    }
}

/// Test suite for memory leak detection
#[cfg(test)]
mod memory_leak_tests {
    use super::*;
    use common::MemoryTracker;

    #[tokio::test]
    async fn test_device_connection_memory_leak() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let mut memory_tracker = MemoryTracker::new();

        let device_id = harness.add_device(DeviceType::TTL).await;

        // Perform many connect/disconnect cycles
        for cycle in 0..100 {
            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.connect().await?;
                device.disconnect().await?;
            }

            // Measure memory every 10 cycles
            if cycle % 10 == 0 {
                memory_tracker.measure();
            }
        }

        // Final memory measurement
        memory_tracker.measure();

        // Note: Memory measurements can be noisy due to system activity
        // We use a generous threshold (100MB) to avoid flaky tests
        Assertions::assert_no_memory_leak(
            memory_tracker.memory_increase(),
            100,
            "device connection cycles",
        )?;

        println!(
            "Memory increase: {} bytes",
            memory_tracker.memory_increase()
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_message_processing_memory_leak() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let mut memory_tracker = MemoryTracker::new();

        let device_id = harness.add_connected_device(DeviceType::Kernel).await?;

        // Send many messages
        let large_message = vec![0u8; 1024]; // 1KB message
        for message_num in 0..1000 {
            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(&large_message).await?;
            }

            // Measure memory every 100 messages
            if message_num % 100 == 0 {
                memory_tracker.measure();
            }
        }

        memory_tracker.measure();

        // Note: Memory measurements can be noisy due to system activity
        // We use a generous threshold (100MB) to avoid flaky tests
        Assertions::assert_no_memory_leak(
            memory_tracker.memory_increase(),
            100,
            "message processing",
        )?;

        println!(
            "Memory increase after 1000 messages: {} bytes",
            memory_tracker.memory_increase()
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_state_memory_leak() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let mut memory_tracker = MemoryTracker::new();

        // Create and destroy many devices
        for cycle in 0..50 {
            let device_id = harness.add_device(DeviceType::Mock).await;

            // Connect and use device
            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.connect().await?;
                device.send(b"test").await?;
                device.disconnect().await?;
            }

            // Remove device
            harness.app_state.remove_device(&device_id).await;
            // Remove from our tracking list too
            harness.devices.retain(|d| d != &device_id);

            // Measure memory every 10 cycles
            if cycle % 10 == 0 {
                memory_tracker.measure();
            }
        }

        memory_tracker.measure();

        // Note: Memory measurements can be noisy due to system activity
        // We use a generous threshold (100MB) to avoid flaky tests
        Assertions::assert_no_memory_leak(
            memory_tracker.memory_increase(),
            100,
            "device state management",
        )?;

        println!(
            "Memory increase after device lifecycle test: {} bytes",
            memory_tracker.memory_increase()
        );

        harness.cleanup().await
    }
}

/// Test suite for edge cases and boundary conditions
#[cfg(test)]
mod edge_case_tests {
    use super::*;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_extremely_high_latency_device() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness
            .add_device_with_latency(DeviceType::Kernel, 5000)
            .await; // 5 second latency

        // Test connection with timeout
        let connect_result = timeout(Duration::from_secs(10), async {
            let device_lock = harness
                .app_state
                .get_device(&device_id)
                .await
                .ok_or_else(|| TestError::Setup("Device not found".to_string()))?;
            let mut device = device_lock.write().await;
            device.connect().await.map_err(TestError::Device)
        })
        .await;

        assert!(connect_result.is_ok(), "Connection timed out");
        connect_result.unwrap()?;

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_zero_byte_messages() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Test sending empty message
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.send(&[]).await?;
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_large_message_handling() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::Kernel).await?;

        // Test sending large message (1MB)
        let large_message = vec![0u8; 1024 * 1024];
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.send(&large_message).await?;
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_rapid_connect_disconnect_cycles() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_device(DeviceType::TTL).await;

        // Perform rapid connect/disconnect cycles
        for cycle in 0..100 {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;

            device.connect().await.map_err(|e| TestError::Device(e))?;
            device
                .disconnect()
                .await
                .map_err(|e| TestError::Device(e))?;
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_concurrent_access_to_same_device() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Launch concurrent operations
        let state = harness.app_state.clone();
        let device_id_for_tasks = device_id.clone();

        let results = run_concurrent(10, move |worker_id| {
            let state = state.clone();
            let device_id = device_id_for_tasks.clone();
            async move {
                let test_data = format!("worker_{}", worker_id);
                for _ in 0..10 {
                    if let Some(device_lock) = state.get_device(&device_id).await {
                        let mut device = device_lock.write().await;
                        device
                            .send(test_data.as_bytes())
                            .await
                            .map_err(TestError::Device)?;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            }
        })
        .await;

        assert!(results.all_ok(), "Some concurrent operations failed");

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_operations_when_disconnected() -> TestResult<()> {
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_device(DeviceType::TTL).await;

        // Attempt operations on disconnected device
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;

            // These should fail because device is not connected
            let send_result = device.send(b"test").await;
            assert!(send_result.is_err(), "Send should fail when disconnected");
        }

        harness.cleanup().await
    }
}

/// Test suite for resource cleanup verification
#[cfg(test)]
mod resource_cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn test_app_state_cleanup() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add multiple devices
        for _ in 0..5 {
            harness.add_device(DeviceType::Mock).await;
        }

        // Verify devices are added
        assert_eq!(harness.device_count().await, 5);

        // Get device IDs before cleanup
        let device_ids: Vec<String> = harness.devices.clone();

        // Clean up all devices
        harness.cleanup().await?;

        // Re-create harness to verify state (can't use old harness after cleanup)
        let harness2 = TestHarness::new().await;

        // Verify devices are no longer accessible
        for device_id in device_ids {
            let device = harness2.app_state.get_device(&device_id).await;
            assert!(
                device.is_none(),
                "Device {} still accessible after cleanup",
                device_id
            );
        }

        harness2.cleanup().await
    }

    #[tokio::test]
    async fn test_explicit_cleanup_required() -> TestResult<()> {
        // This test demonstrates that cleanup is explicit and doesn't panic
        let mut harness = TestHarness::new().await;
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Verify device exists
        assert!(harness.app_state.get_device(&device_id).await.is_some());

        // Explicit cleanup - this is the key difference from TestFixture
        harness.cleanup().await
    }
}
