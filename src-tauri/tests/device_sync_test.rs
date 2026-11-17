//! Device synchronization and time alignment tests
//!
//! Uses the new TestHarness infrastructure for explicit async cleanup.

mod common;
use common::prelude::*;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Test suite for multi-device synchronization
#[cfg(test)]
mod multi_device_sync_tests {
    use super::*;

    #[tokio::test]
    async fn test_synchronized_device_operations() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create multiple devices of different types
        let devices = harness.add_connected_multi_device_setup().await?;

        // Perform synchronized operations across all devices
        let sync_operation_data = b"sync_test_data";
        let operation_start = Instant::now();

        // Send command to all devices simultaneously
        let mut operation_tasks = Vec::new();
        for (device_type, device_id) in &devices {
            let state = harness.app_state.clone();
            let device_id_clone = device_id.clone();
            let task = tokio::spawn(async move {
                let operation_time = Instant::now();
                if let Some(device_lock) = state.get_device(&device_id_clone).await {
                    let mut device = device_lock.write().await;
                    let result = device.send(sync_operation_data).await;
                    (device_id_clone, operation_time, result)
                } else {
                    (
                        device_id_clone,
                        operation_time,
                        Err(DeviceError::NotConnected),
                    )
                }
            });
            operation_tasks.push(task);
        }

        // Collect results
        let mut operation_results = Vec::new();
        for task in operation_tasks {
            let (device_id, operation_time, result) = task
                .await
                .map_err(|e| TestError::TaskFailed(e.to_string()))?;
            operation_results.push((device_id, operation_time, result));
        }

        let total_operation_time = operation_start.elapsed();

        // Verify all operations completed successfully
        for (device_id, _, result) in &operation_results {
            result.as_ref().map_err(|e| {
                TestError::Assertion(format!(
                    "Operation failed for device {}: {:?}",
                    device_id, e
                ))
            })?;
        }

        // Verify operations were reasonably synchronized (within 100ms of each other)
        let operation_times: Vec<_> = operation_results
            .iter()
            .map(|(_, time, _)| time.duration_since(operation_start))
            .collect();

        let min_time = operation_times.iter().min().unwrap();
        let max_time = operation_times.iter().max().unwrap();
        let time_spread = max_time.saturating_sub(*min_time);

        println!(
            "Multi-device sync: {} devices, time spread: {:?}, total time: {:?}",
            devices.len(),
            time_spread,
            total_operation_time
        );

        Assertions::assert_latency(time_spread, 100.0, "device operation synchronization")?;

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_ordered_device_operations() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create a sequence of devices for ordered operations
        let device_count = 5;
        let mut device_ids = Vec::new();

        for _ in 0..device_count {
            let device_id = harness.add_connected_device(DeviceType::Mock).await?;
            device_ids.push(device_id);
        }

        // Perform ordered operations with specific timing
        let mut operation_times = Vec::new();
        let operation_interval = Duration::from_millis(50);

        for (index, device_id) in device_ids.iter().enumerate() {
            let operation_data = format!("order_{}", index);
            let operation_start = Instant::now();

            {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(operation_data.as_bytes()).await?;
            }

            operation_times.push(operation_start);

            // Wait for next operation
            if index < device_ids.len() - 1 {
                tokio::time::sleep(operation_interval).await;
            }
        }

        // Verify operations were performed in order with correct timing
        for i in 1..operation_times.len() {
            let interval = operation_times[i].duration_since(operation_times[i - 1]);

            // Allow for some timing variance (±15ms)
            Assertions::assert_duration_approx(
                interval,
                operation_interval,
                Duration::from_millis(15),
                &format!("interval between operations {} and {}", i - 1, i),
            )?;
        }

        println!("Ordered operations completed with proper timing intervals");

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_device_state_consistency() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create multiple devices
        let mut device_ids = Vec::new();
        for _ in 0..10 {
            let device_id = harness.add_device(DeviceType::Mock).await;
            device_ids.push(device_id);
        }

        // Perform rapid state changes across all devices
        for cycle in 0..5 {
            // Connect all devices
            for device_id in &device_ids {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.connect().await?;
            }

            // Verify all devices are connected
            for device_id in &device_ids {
                Assertions::assert_device_status(
                    &harness,
                    device_id,
                    DeviceStatus::Connected,
                    &format!("cycle {} after connect", cycle),
                )
                .await?;
            }

            // Perform operations on all devices
            for device_id in &device_ids {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(b"state_test").await?;
            }

            // Disconnect all devices
            for device_id in &device_ids {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.disconnect().await?;
            }

            // Verify all devices are disconnected
            for device_id in &device_ids {
                Assertions::assert_device_status(
                    &harness,
                    device_id,
                    DeviceStatus::Disconnected,
                    &format!("cycle {} after disconnect", cycle),
                )
                .await?;
            }
        }

        // Verify final state consistency
        assert_eq!(harness.device_count().await, device_ids.len());

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_concurrent_multi_device_access() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create devices for concurrent access testing
        let device_types = vec![DeviceType::TTL, DeviceType::Kernel, DeviceType::Pupil];
        let mut device_ids = Vec::new();

        for device_type in device_types {
            let device_id = harness.add_connected_device(device_type).await?;
            device_ids.push(device_id);
        }

        // Launch concurrent operations on all devices
        let concurrent_workers = 5;
        let operations_per_worker = 20;

        let state_for_test = harness.app_state.clone();
        let device_ids_for_test = device_ids.clone();

        let results = run_load_test(
            concurrent_workers,
            operations_per_worker,
            move |worker_id, _op_id| {
                let state = state_for_test.clone();
                let device_ids = device_ids_for_test.clone();
                async move {
                    let device_id = &device_ids[worker_id % device_ids.len()];
                    let operation_data = format!("worker_{}_{}", worker_id, rand::random::<u32>());

                    let start = Instant::now();
                    if let Some(device_lock) = state.get_device(device_id).await {
                        let mut device = device_lock.write().await;
                        device
                            .send(operation_data.as_bytes())
                            .await
                            .map_err(TestError::Device)?;
                    }
                    Ok(start.elapsed())
                }
            },
        )
        .await;

        // Analyze concurrent access performance
        let flattened = results.flatten();
        let latencies = flattened.unwrap_all();
        let stats = LatencyStats::from_latencies(&latencies).unwrap();

        println!("Concurrent multi-device access:");
        println!(
            "  Workers: {}, Operations per worker: {}",
            concurrent_workers, operations_per_worker
        );
        println!("  Average latency: {:?}", stats.avg);
        println!("  Max latency: {:?}", stats.max);

        // Verify reasonable performance under concurrent access
        Assertions::assert_latency(stats.avg, 50.0, "average under concurrent access")?;
        Assertions::assert_latency(stats.max, 200.0, "maximum under concurrent access")?;

        harness.cleanup().await
    }
}

/// Test suite for time alignment verification
#[cfg(test)]
// These tests time real sleeps between sends to mock devices and assert tight limits (10-20 ms, 10% rate). They
// measure how evenly the host wakes a sleeping task, so one late wake-up on a shared CI runner fails them without
// any Bridge code being slow (seen on macOS CI: 64 ms spread, 15 ms deviation). They are ignored by default.
mod time_alignment_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "depends on host scheduling; run on a quiet machine with --ignored"]
    async fn test_device_timestamp_synchronization() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add devices for timestamp testing
        let mut device_ids = Vec::new();
        for _ in 0..3 {
            let device_id = harness.add_connected_device(DeviceType::Mock).await?;
            device_ids.push(device_id);
        }

        // Record timestamps for synchronized operations
        let sync_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Perform simultaneous operations and record timestamps
        let mut sync_tasks = Vec::new();
        for device_id in &device_ids {
            let state = harness.app_state.clone();
            let device_id_clone = device_id.clone();
            let task = tokio::spawn(async move {
                let operation_timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                if let Some(device_lock) = state.get_device(&device_id_clone).await {
                    let mut device = device_lock.write().await;
                    device.send(b"timestamp_test").await.unwrap();
                }

                (device_id_clone, operation_timestamp)
            });
            sync_tasks.push(task);
        }

        // Collect timestamps
        let mut operation_timestamps = Vec::new();
        for task in sync_tasks {
            let (device_id, timestamp) = task
                .await
                .map_err(|e| TestError::TaskFailed(e.to_string()))?;
            operation_timestamps.push((device_id, timestamp));
        }

        // Verify timestamp alignment (should be within 50ms of sync timestamp)
        for (device_id, timestamp) in &operation_timestamps {
            let time_diff = timestamp.abs_diff(sync_timestamp);
            if time_diff >= 50 {
                return Err(TestError::Assertion(format!(
                    "Device {} timestamp not aligned: {}ms difference",
                    device_id, time_diff
                )));
            }
        }

        // Verify timestamps are close to each other (within 20ms)
        let timestamps: Vec<_> = operation_timestamps.iter().map(|(_, ts)| *ts).collect();
        let min_timestamp = timestamps.iter().min().unwrap();
        let max_timestamp = timestamps.iter().max().unwrap();
        let timestamp_spread = max_timestamp - min_timestamp;

        println!("Timestamp synchronization:");
        println!("  Devices: {}", device_ids.len());
        println!("  Timestamp spread: {}ms", timestamp_spread);

        if timestamp_spread >= 20 {
            return Err(TestError::Assertion(format!(
                "Device timestamps not well synchronized: {}ms spread",
                timestamp_spread
            )));
        }

        harness.cleanup().await
    }

    #[tokio::test]
    #[ignore = "depends on host scheduling; run on a quiet machine with --ignored"]
    async fn test_operation_timing_precision() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add TTL device for precise timing testing
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Measure operation timing precision
        let target_intervals = vec![10, 50, 100]; // milliseconds
        let measurements_per_interval = 10;

        for target_interval_ms in target_intervals {
            let target_interval = Duration::from_millis(target_interval_ms);
            let mut actual_intervals = Vec::new();

            let mut last_operation_time = Instant::now();

            for _ in 0..measurements_per_interval {
                tokio::time::sleep(target_interval).await;

                let operation_time = Instant::now();
                {
                    let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                    let mut device = device_lock.write().await;
                    device.send(b"PULSE").await?;
                }

                let actual_interval = operation_time.duration_since(last_operation_time);
                actual_intervals.push(actual_interval);
                last_operation_time = operation_time;
            }

            // Analyze timing precision
            let avg_interval =
                actual_intervals.iter().sum::<Duration>() / actual_intervals.len() as u32;
            let max_deviation = actual_intervals
                .iter()
                .map(|interval| interval.as_millis().abs_diff(target_interval.as_millis()))
                .max()
                .unwrap();

            println!("Timing precision for {}ms intervals:", target_interval_ms);
            println!("  Average interval: {:?}", avg_interval);
            println!("  Max deviation: {}ms", max_deviation);

            // Allow ±10ms deviation for intervals >= 10ms
            let allowed_deviation = 10;
            if max_deviation > allowed_deviation {
                return Err(TestError::Assertion(format!(
                    "Timing precision too low for {}ms intervals: {}ms max deviation",
                    target_interval_ms, max_deviation
                )));
            }
        }

        harness.cleanup().await
    }

    #[tokio::test]
    #[ignore = "depends on host scheduling; run on a quiet machine with --ignored"]
    async fn test_cross_device_timing_correlation() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create devices for timing correlation testing
        let device1_id = harness.add_connected_device(DeviceType::TTL).await?;
        let device2_id = harness.add_connected_device(DeviceType::Kernel).await?;
        let device3_id = harness.add_connected_device(DeviceType::Pupil).await?;

        let device_ids = vec![device1_id, device2_id, device3_id];

        // Perform correlated operations across devices
        let correlation_count = 20;
        let mut correlation_data = Vec::new();

        for sequence in 0..correlation_count {
            let sequence_start = Instant::now();
            let mut device_timings = Vec::new();

            // Send operations to all devices in sequence
            for (index, device_id) in device_ids.iter().enumerate() {
                let operation_start = Instant::now();
                {
                    let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                    let mut device = device_lock.write().await;
                    device
                        .send(format!("seq_{}_{}", sequence, index).as_bytes())
                        .await?;
                }
                let operation_duration = operation_start.elapsed();
                device_timings.push((device_id.clone(), operation_start, operation_duration));

                // Small delay between device operations
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            correlation_data.push((sequence_start, device_timings));
        }

        // Analyze timing correlations
        let mut inter_device_intervals = Vec::new();

        for (_, device_timings) in &correlation_data {
            for i in 1..device_timings.len() {
                let prev_time = device_timings[i - 1].1;
                let curr_time = device_timings[i].1;
                let interval = curr_time.duration_since(prev_time);
                inter_device_intervals.push(interval);
            }
        }

        // Verify consistent inter-device timing
        let avg_interval =
            inter_device_intervals.iter().sum::<Duration>() / inter_device_intervals.len() as u32;
        let min_interval = inter_device_intervals.iter().min().unwrap();
        let max_interval = inter_device_intervals.iter().max().unwrap();
        let interval_variance = max_interval.saturating_sub(*min_interval);

        println!("Cross-device timing correlation:");
        println!("  Average inter-device interval: {:?}", avg_interval);
        println!("  Interval variance: {:?}", interval_variance);

        // Verify consistent timing (variance should be small)
        Assertions::assert_latency(interval_variance, 20.0, "inter-device timing variance")?;

        harness.cleanup().await
    }

    #[tokio::test]
    #[ignore = "depends on host scheduling; run on a quiet machine with --ignored"]
    async fn test_long_term_timing_stability() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Add device for long-term timing test
        let device_id = harness.add_connected_device(DeviceType::TTL).await?;

        // Perform operations over a longer period to test timing stability
        let test_duration = Duration::from_secs(5);
        let operation_interval = Duration::from_millis(100);
        let expected_operations = test_duration.as_millis() / operation_interval.as_millis();

        let start_time = Instant::now();
        let mut operation_times = Vec::new();
        let mut operation_count = 0;

        while start_time.elapsed() < test_duration {
            let operation_time = Instant::now();

            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(b"stability_test").await?;
            }

            operation_times.push(operation_time);
            operation_count += 1;

            tokio::time::sleep(operation_interval).await;
        }

        let actual_duration = start_time.elapsed();

        // Analyze timing stability
        let actual_operation_rate = operation_count as f64 / actual_duration.as_secs_f64();
        let expected_operation_rate = 1000.0 / operation_interval.as_millis() as f64;

        println!("Long-term timing stability:");
        println!("  Duration: {:?}", actual_duration);
        println!(
            "  Operations: {} (expected ~{})",
            operation_count, expected_operations
        );
        println!(
            "  Operation rate: {:.1} Hz (expected {:.1} Hz)",
            actual_operation_rate, expected_operation_rate
        );

        // Verify timing stability
        let rate_error =
            (actual_operation_rate - expected_operation_rate).abs() / expected_operation_rate;
        if rate_error > 0.10 {
            return Err(TestError::Assertion(format!(
                "Operation rate drift too high: {:.1}% error",
                rate_error * 100.0
            )));
        }

        harness.cleanup().await
    }
}

/// Test suite for data integrity across devices
#[cfg(test)]
mod data_integrity_tests {
    use super::*;

    #[tokio::test]
    async fn test_data_consistency_across_devices() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create multiple devices for data consistency testing
        let mut device_ids = Vec::new();
        for _ in 0..5 {
            let device_id = harness.add_connected_device(DeviceType::Mock).await?;
            device_ids.push(device_id);
        }

        // Generate test data for consistency verification
        let test_data_sets = vec![
            b"consistency_test_1".to_vec(),
            b"consistency_test_2".to_vec(),
            b"consistency_test_3".to_vec(),
        ];

        // Send same data to all devices and verify consistency
        for (data_index, test_data) in test_data_sets.iter().enumerate() {
            // Send data to all devices
            for device_id in &device_ids {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(test_data).await?;
            }

            // Verify data was recorded consistently across all devices
            for device_id in &device_ids {
                let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                let device = device_lock.read().await;
                if let Some(mock_device) = device
                    .as_any()
                    .and_then(|any| any.downcast_ref::<TestMockDevice>())
                {
                    let sent_data = mock_device.get_sent_data().await;
                    if sent_data.len() < data_index + 1 {
                        return Err(TestError::Assertion(format!(
                            "Device {} missing data for index {}",
                            device_id, data_index
                        )));
                    }
                    if sent_data[data_index] != *test_data {
                        return Err(TestError::Assertion(format!(
                            "Data inconsistency in device {} at index {}",
                            device_id, data_index
                        )));
                    }
                }
            }
        }

        println!(
            "Data consistency verified across {} devices for {} data sets",
            device_ids.len(),
            test_data_sets.len()
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_data_ordering_preservation() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_connected_device(DeviceType::Mock).await?;

        // Send ordered data and verify order preservation
        let ordered_data: Vec<Vec<u8>> = (0..20)
            .map(|i| format!("order_{:03}", i).into_bytes())
            .collect();

        // Send data in order
        for data in &ordered_data {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.send(data).await?;
        }

        // Verify order preservation
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let sent_data = mock_device.get_sent_data().await;

                if sent_data.len() != ordered_data.len() {
                    return Err(TestError::Assertion(format!(
                        "Incorrect number of data items: {} vs expected {}",
                        sent_data.len(),
                        ordered_data.len()
                    )));
                }

                for (index, expected_data) in ordered_data.iter().enumerate() {
                    if sent_data[index] != *expected_data {
                        return Err(TestError::Assertion(format!(
                            "Data order not preserved at index {}",
                            index
                        )));
                    }
                }
            }
        }

        println!(
            "Data ordering preserved for {} sequential items",
            ordered_data.len()
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_data_integrity_under_concurrent_access() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_connected_device(DeviceType::Mock).await?;

        // Perform concurrent data operations
        let concurrent_workers = 5;
        let data_per_worker = 10;

        let state = harness.app_state.clone();
        let device_id_for_tasks = device_id.clone();

        let results = run_concurrent(concurrent_workers, move |worker_id| {
            let state = state.clone();
            let device_id = device_id_for_tasks.clone();

            async move {
                let mut worker_data = Vec::new();
                for data_index in 0..data_per_worker {
                    let data = format!("worker_{}_{:03}", worker_id, data_index).into_bytes();
                    worker_data.push(data.clone());

                    if let Some(device_lock) = state.get_device(&device_id).await {
                        let mut device = device_lock.write().await;
                        device.send(&data).await.map_err(TestError::Device)?;
                    }

                    // Small delay to increase chance of interleaving
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok(worker_data)
            }
        })
        .await;

        // Collect expected data from all workers
        let all_expected_data: Vec<Vec<u8>> = results.unwrap_all().into_iter().flatten().collect();

        // Verify data integrity
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let received_data = mock_device.get_sent_data().await;

                if received_data.len() != all_expected_data.len() {
                    return Err(TestError::Assertion(format!(
                        "Data count mismatch under concurrent access: {} vs expected {}",
                        received_data.len(),
                        all_expected_data.len()
                    )));
                }

                // Verify all expected data was received (order may vary due to concurrency)
                for expected_item in &all_expected_data {
                    if !received_data.contains(expected_item) {
                        return Err(TestError::Assertion(format!(
                            "Missing data item: {:?}",
                            String::from_utf8_lossy(expected_item)
                        )));
                    }
                }
            }
        }

        println!(
            "Data integrity verified under concurrent access: {} workers, {} items per worker",
            concurrent_workers, data_per_worker
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_large_data_transfer_integrity() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_connected_device(DeviceType::Kernel).await?;

        // Test data integrity for large transfers
        let data_sizes = vec![1024, 10240, 102400]; // 1KB, 10KB, 100KB

        for data_size in data_sizes {
            // Generate large data with verifiable pattern
            let mut large_data = Vec::with_capacity(data_size);
            for i in 0..data_size {
                large_data.push((i % 256) as u8);
            }

            // Add checksum to data
            let checksum: u32 = large_data.iter().map(|&b| b as u32).sum();
            let checksum_bytes = checksum.to_be_bytes();
            large_data.extend_from_slice(&checksum_bytes);

            // Send large data
            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(&large_data).await?;
            }

            // Verify data integrity
            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let device = device_lock.read().await;
                if let Some(mock_device) = device
                    .as_any()
                    .and_then(|any| any.downcast_ref::<TestMockDevice>())
                {
                    let sent_data = mock_device.get_sent_data().await;
                    let received_data = sent_data.last().unwrap();

                    if received_data.len() != large_data.len() {
                        return Err(TestError::Assertion(format!(
                            "Large data size mismatch: {} bytes vs expected {}",
                            received_data.len(),
                            large_data.len()
                        )));
                    }

                    // Verify data content
                    if *received_data != large_data {
                        return Err(TestError::Assertion(format!(
                            "Large data content corruption for {} byte transfer",
                            data_size
                        )));
                    }

                    // Verify checksum
                    let received_checksum_bytes = &received_data[data_size..data_size + 4];
                    let received_checksum = u32::from_be_bytes([
                        received_checksum_bytes[0],
                        received_checksum_bytes[1],
                        received_checksum_bytes[2],
                        received_checksum_bytes[3],
                    ]);
                    if received_checksum != checksum {
                        return Err(TestError::Assertion(format!(
                            "Checksum mismatch for {} byte transfer",
                            data_size
                        )));
                    }
                }
            }

            println!(
                "Large data transfer integrity verified: {} bytes",
                data_size
            );
        }

        harness.cleanup().await
    }
}

/// Test suite for LSL stream integration
#[cfg(test)]
mod lsl_integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_lsl_mock_stream_setup() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create mock LSL device for integration testing
        let lsl_device_id = harness.add_connected_device(DeviceType::LSL).await?;

        // Test LSL stream simulation
        let stream_data_samples = vec![
            b"lsl_sample_1".to_vec(),
            b"lsl_sample_2".to_vec(),
            b"lsl_sample_3".to_vec(),
        ];

        for sample in &stream_data_samples {
            let device_lock = harness.app_state.get_device(&lsl_device_id).await.unwrap();
            let mut device = device_lock.write().await;
            device.send(sample).await?;
        }

        // Verify LSL stream data handling
        {
            let device_lock = harness.app_state.get_device(&lsl_device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let sent_data = mock_device.get_sent_data().await;
                assert_eq!(sent_data.len(), stream_data_samples.len());

                for (index, expected_sample) in stream_data_samples.iter().enumerate() {
                    assert_eq!(sent_data[index], *expected_sample);
                }
            }
        }

        println!("LSL mock stream integration test completed");

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_multi_stream_synchronization() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create multiple LSL-like devices for multi-stream testing
        let stream_count = 3;
        let mut stream_device_ids = Vec::new();

        for _ in 0..stream_count {
            let device_id = harness.add_connected_device(DeviceType::LSL).await?;
            stream_device_ids.push(device_id);
        }

        // Simulate synchronized streaming across multiple streams
        let samples_per_stream = 10;
        let sync_interval = Duration::from_millis(50);

        for sample_index in 0..samples_per_stream {
            let sync_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();

            // Send synchronized samples to all streams
            let mut stream_tasks = Vec::new();

            for (stream_id, device_id) in stream_device_ids.iter().enumerate() {
                let state = harness.app_state.clone();
                let device_id_clone = device_id.clone();
                let sample_data = format!(
                    "stream_{}_sample_{}_ts_{}",
                    stream_id, sample_index, sync_timestamp
                );

                let task = tokio::spawn(async move {
                    if let Some(device_lock) = state.get_device(&device_id_clone).await {
                        let mut device = device_lock.write().await;
                        device.send(sample_data.as_bytes()).await.unwrap();
                    }
                    sync_timestamp
                });
                stream_tasks.push(task);
            }

            // Wait for all streams to complete this sample
            let mut sample_timestamps = Vec::new();
            for task in stream_tasks {
                let timestamp = task
                    .await
                    .map_err(|e| TestError::TaskFailed(e.to_string()))?;
                sample_timestamps.push(timestamp);
            }

            // Verify timestamp synchronization across streams
            let min_ts = sample_timestamps.iter().min().unwrap();
            let max_ts = sample_timestamps.iter().max().unwrap();
            let timestamp_spread = max_ts - min_ts;

            if timestamp_spread > 10 {
                return Err(TestError::Assertion(format!(
                    "Stream timestamps not synchronized: {}ms spread",
                    timestamp_spread
                )));
            }

            tokio::time::sleep(sync_interval).await;
        }

        println!(
            "Multi-stream synchronization test completed: {} streams, {} samples per stream",
            stream_count, samples_per_stream
        );

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_stream_data_buffering() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        let device_id = harness.add_connected_device(DeviceType::LSL).await?;

        // Test stream data buffering under high-frequency data
        let high_frequency_samples = 1000;
        let sample_interval = Duration::from_micros(500); // 2kHz sampling rate

        let start_time = Instant::now();

        for sample_id in 0..high_frequency_samples {
            let sample_data = format!("hf_sample_{:06}", sample_id);

            {
                let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
                let mut device = device_lock.write().await;
                device.send(sample_data.as_bytes()).await?;
            }

            if sample_id % 100 == 0 {
                tokio::time::sleep(sample_interval).await;
            }
        }

        let total_time = start_time.elapsed();
        let actual_sample_rate = high_frequency_samples as f64 / total_time.as_secs_f64();

        // Verify all samples were buffered correctly
        {
            let device_lock = harness.app_state.get_device(&device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let buffered_data = mock_device.get_sent_data().await;

                if buffered_data.len() != high_frequency_samples {
                    return Err(TestError::Assertion(format!(
                        "Sample count mismatch in buffering test: {} vs expected {}",
                        buffered_data.len(),
                        high_frequency_samples
                    )));
                }

                // Verify sample order and content
                for (index, sample_data) in buffered_data.iter().enumerate() {
                    let expected_sample = format!("hf_sample_{:06}", index);
                    if *sample_data != expected_sample.as_bytes() {
                        return Err(TestError::Assertion(format!(
                            "Sample data mismatch at index {}",
                            index
                        )));
                    }
                }
            }
        }

        println!(
            "Stream buffering test: {} samples at {:.0} Hz",
            high_frequency_samples, actual_sample_rate
        );

        harness.cleanup().await
    }
}

/// Test suite for cross-device event correlation
#[cfg(test)]
mod event_correlation_tests {
    use super::*;

    #[tokio::test]
    async fn test_cross_device_event_triggering() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create devices for event correlation testing
        let ttl_device_id = harness.add_connected_device(DeviceType::TTL).await?;
        let kernel_device_id = harness.add_connected_device(DeviceType::Kernel).await?;
        let pupil_device_id = harness.add_connected_device(DeviceType::Pupil).await?;

        let device_ids = vec![ttl_device_id, kernel_device_id, pupil_device_id];

        // Test event correlation across devices
        let event_sequences = vec![
            ("trigger_start", vec![0, 1, 2]),  // TTL -> Kernel -> Pupil
            ("marker_event", vec![1, 0]),      // Kernel -> TTL
            ("recording_sync", vec![2, 1, 0]), // Pupil -> Kernel -> TTL
        ];

        for (event_type, device_sequence) in event_sequences {
            let event_id = Uuid::new_v4();
            let sequence_start = Instant::now();

            println!("Testing event correlation: {}", event_type);

            for (step, &device_index) in device_sequence.iter().enumerate() {
                let device_id = &device_ids[device_index];
                let event_data = format!("{}_{}_step_{}", event_type, event_id, step);

                let step_start = Instant::now();

                {
                    let device_lock = harness.app_state.get_device(device_id).await.unwrap();
                    let mut device = device_lock.write().await;
                    device.send(event_data.as_bytes()).await?;
                }

                let step_duration = step_start.elapsed();

                // Verify event timing (each step should complete quickly)
                Assertions::assert_latency(step_duration, 50.0, &format!("event step {}", step))?;

                // Small delay between correlated events
                if step < device_sequence.len() - 1 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }

            let total_sequence_time = sequence_start.elapsed();
            println!("  Sequence completed in: {:?}", total_sequence_time);

            // Verify sequence completed in reasonable time
            Assertions::assert_latency(total_sequence_time, 200.0, "event sequence")?;
        }

        // Verify all devices participated
        for (device_index, device_id) in device_ids.iter().enumerate() {
            let device_lock = harness.app_state.get_device(device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let device_data = mock_device.get_sent_data().await;

                if device_data.is_empty() {
                    return Err(TestError::Assertion(format!(
                        "Device {} received no events",
                        device_index
                    )));
                }

                println!(
                    "Device {} received {} events",
                    device_index,
                    device_data.len()
                );
            }
        }

        harness.cleanup().await
    }

    #[tokio::test]
    async fn test_complex_event_workflow() -> TestResult<()> {
        let mut harness = TestHarness::new().await;

        // Create a complex multi-device workflow scenario
        let ttl_device_id = harness.add_connected_device(DeviceType::TTL).await?;
        let kernel_device_id = harness.add_connected_device(DeviceType::Kernel).await?;
        let pupil_device_id = harness.add_connected_device(DeviceType::Pupil).await?;

        let all_devices = vec![
            ("TTL", ttl_device_id),
            ("Kernel", kernel_device_id),
            ("Pupil", pupil_device_id),
        ];

        // Define complex workflow: Experiment session simulation
        let workflow_steps = vec![
            ("session_start", vec![0]),      // TTL triggers session start
            ("recording_begin", vec![1, 2]), // Start recording on all data devices
            ("trial_start", vec![0]),        // TTL marks trial start
            ("stimulus_present", vec![1]),   // Kernel presents stimulus
            ("response_capture", vec![2]),   // Pupil captures response
            ("trial_end", vec![0]),          // TTL marks trial end
            ("recording_end", vec![1, 2]),   // Stop recording
            ("session_end", vec![0]),        // TTL ends session
        ];

        let workflow_start = Instant::now();

        for (step_name, device_indices) in workflow_steps {
            let step_start = Instant::now();

            println!("Executing workflow step: {}", step_name);

            // Execute step across specified devices
            let mut step_tasks = Vec::new();

            for &device_index in &device_indices {
                let (device_name, device_id) = &all_devices[device_index];
                let state = harness.app_state.clone();
                let device_id_clone = device_id.clone();
                let step_data = format!("{}_{}", step_name, device_name);

                let task = tokio::spawn(async move {
                    if let Some(device_lock) = state.get_device(&device_id_clone).await {
                        let mut device = device_lock.write().await;
                        device.send(step_data.as_bytes()).await.unwrap();
                    }
                    device_id_clone
                });
                step_tasks.push(task);
            }

            // Wait for step completion
            for task in step_tasks {
                task.await
                    .map_err(|e| TestError::TaskFailed(e.to_string()))?;
            }

            let step_duration = step_start.elapsed();
            println!("  Step '{}' completed in: {:?}", step_name, step_duration);

            // Verify step completed quickly
            Assertions::assert_latency(
                step_duration,
                100.0,
                &format!("workflow step '{}'", step_name),
            )?;

            // Inter-step delay for realistic workflow timing
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let total_workflow_time = workflow_start.elapsed();
        println!("Complete workflow executed in: {:?}", total_workflow_time);

        // Verify workflow completed in reasonable time
        if total_workflow_time.as_secs() >= 5 {
            return Err(TestError::Assertion(format!(
                "Workflow took too long: {:?}",
                total_workflow_time
            )));
        }

        // Verify all devices participated in workflow
        for (device_name, device_id) in &all_devices {
            let device_lock = harness.app_state.get_device(device_id).await.unwrap();
            let device = device_lock.read().await;
            if let Some(mock_device) = device
                .as_any()
                .and_then(|any| any.downcast_ref::<TestMockDevice>())
            {
                let device_data = mock_device.get_sent_data().await;
                println!(
                    "Device {} executed {} workflow commands",
                    device_name,
                    device_data.len()
                );
                if device_data.is_empty() {
                    return Err(TestError::Assertion(format!(
                        "Device {} did not participate in workflow",
                        device_name
                    )));
                }
            }
        }

        harness.cleanup().await
    }
}
