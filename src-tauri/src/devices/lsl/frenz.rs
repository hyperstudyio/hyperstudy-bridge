//! Earable FRENZ Brainband LSL Integration
//!
//! This module provides automatic discovery and data consumption from FRENZ brainband
//! devices streaming via Lab Streaming Layer (LSL) through the Python `frenztoolkit` bridge.
//!
//! # Architecture
//!
//! ```text
//! FRENZ Band ──BLE──> Python (frenztoolkit + pylsl) ──LSL──> HyperStudy Bridge (Rust LSL inlet)
//!                                                     LSL <── HyperStudy Bridge (marker outlet)
//! ```
//!
//! # FRENZ LSL Streams
//!
//! The Python bridge creates up to 23 streams per device, all named `{DEVICE_ID}_{suffix}`:
//! - Raw signals: `_EEG_raw`, `_PPG_raw`, `_IMU_raw`
//! - Filtered signals: `_EEG_filtered`, `_EOG_filtered`, `_EMG_filtered`
//! - Derived metrics: `_focus`, `_sleep_stage`, `_poas`, `_POSTURE`, `_signal_quality`
//! - Power bands: `_alpha`, `_beta`, `_theta`, `_gamma`, `_delta`
//!
//! # Usage
//!
//! ```ignore
//! let manager = FrenzLslManager::new(resolver, inlet_manager, outlet_manager);
//!
//! // Discover FRENZ devices
//! let devices = manager.discover_frenz_devices().await?;
//!
//! // Connect to specific streams
//! let eeg_rx = manager.connect_stream("FRENZ_ABC", "_EEG_raw").await?;
//!
//! // Or connect all discovered streams at once
//! let receivers = manager.connect_all_streams("FRENZ_ABC").await?;
//! ```

use super::inlet::{InletConfig, InletManager};
use super::outlet::OutletManager;
use super::resolver::{StreamFilter, StreamResolver};
use super::types::{
    frenz_stream_category, ChannelFormat, DiscoveredFrenzDevice, FrenzStreamInfo, LslError, Sample,
    SampleData, StreamInfo, StreamType,
};
use futures_util::future::join_all;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

/// A sample received from a FRENZ LSL stream.
///
/// This is the generic data type sent through the channel for all FRENZ streams.
/// Numeric streams use `values` (Float32 or Float64 converted to f64).
/// The POSTURE stream uses `string_value`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrenzSample {
    /// Original typed sample; integer streams are never coerced into float64.
    pub data: SampleData,
    /// LSL timestamp
    pub timestamp: f64,
    /// The stream suffix identifying the data type (e.g., "_EEG_raw")
    pub stream_suffix: String,
    /// Numeric sample values (empty for string-only streams like POSTURE)
    pub values: Vec<f64>,
    /// String value (only populated for POSTURE stream)
    pub string_value: Option<String>,
}

impl FrenzSample {
    /// Numeric bytes avoid JSON/JavaScript rounding of int64 and non-finite floats.
    pub fn wire_data(&self) -> serde_json::Value {
        use base64::Engine;
        let format = match &self.data {
            SampleData::Float32(_) => "float32",
            SampleData::Float64(_) => "float64",
            SampleData::Int64(_) => "int64",
            SampleData::Int32(_) => "int32",
            SampleData::Int16(_) => "int16",
            SampleData::Int8(_) => "int8",
            SampleData::String(values) => {
                return serde_json::json!({
                "format": "string", "encoding": "json", "values": values })
            }
        };
        serde_json::json!({"format": format, "encoding": "base64-le",
            "values": base64::engine::general_purpose::STANDARD.encode(self.data.to_bytes())})
    }
}

/// Handle for an active FRENZ stream reading task
struct FrenzStreamTask {
    /// Keep the inlet identity even if discovery replaces its cache.
    inlet_uid: String,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Task join handle
    _handle: tokio::task::JoinHandle<()>,
}

/// Determine appropriate channel buffer size based on stream sampling rate
fn buffer_size_for_rate(nominal_srate: f64) -> usize {
    if nominal_srate >= 100.0 {
        1000 // 125Hz streams: ~8 seconds buffer
    } else if nominal_srate >= 25.0 {
        500 // 25-50Hz streams: ~10-20 seconds buffer
    } else if nominal_srate >= 1.0 {
        250 // Low-rate streams
    } else {
        50 // Very low rate / irregular streams
    }
}

/// FRENZ LSL Manager
///
/// Manages discovery and data streaming from Earable FRENZ brainband devices via LSL.
/// Uses the existing LSL infrastructure (resolver, inlet manager, outlet manager)
/// with FRENZ-specific discovery patterns and generic stream handling.
pub struct FrenzLslManager {
    /// Serialize lifecycle requests; individual inlets still open/close concurrently.
    lifecycle: Mutex<()>,
    /// Stream resolver for discovery (shared with Neon manager)
    resolver: Arc<StreamResolver>,
    /// Inlet manager for consuming streams (shared with Neon manager)
    inlet_manager: Arc<InletManager>,
    /// Outlet manager for creating marker outlets
    outlet_manager: Arc<OutletManager>,
    /// Discovered FRENZ devices (device_name -> DiscoveredFrenzDevice)
    discovered_devices: Arc<RwLock<HashMap<String, DiscoveredFrenzDevice>>>,
    /// Active stream tasks keyed by (device_name, stream_suffix)
    stream_tasks: Arc<RwLock<HashMap<(String, String), FrenzStreamTask>>>,
    /// Marker outlet ID (created once, shared across all FRENZ devices)
    marker_outlet_id: Arc<RwLock<Option<String>>>,
}

impl FrenzLslManager {
    /// Create a new FRENZ LSL manager
    ///
    /// # Arguments
    /// * `resolver` - Stream resolver for LSL discovery (shared to maintain cache)
    /// * `inlet_manager` - Inlet manager for consuming LSL streams
    /// * `outlet_manager` - Outlet manager for creating marker outlets
    pub fn new(
        resolver: Arc<StreamResolver>,
        inlet_manager: Arc<InletManager>,
        outlet_manager: Arc<OutletManager>,
    ) -> Self {
        Self {
            lifecycle: Mutex::new(()),
            resolver,
            inlet_manager,
            outlet_manager,
            discovered_devices: Arc::new(RwLock::new(HashMap::new())),
            stream_tasks: Arc::new(RwLock::new(HashMap::new())),
            marker_outlet_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Discover FRENZ devices streaming via LSL
    ///
    /// Scans the network for LSL streams matching FRENZ patterns and groups them
    /// by device name. Returns a list of discovered devices with their available streams.
    pub async fn discover_frenz_devices(&self) -> Result<Vec<DiscoveredFrenzDevice>, LslError> {
        info!(device = "frenz", "Discovering FRENZ devices via LSL...");

        // Discover all streams (maintains cache for subsequent connect calls)
        let all_streams = self.resolver.discover_streams().await?;

        // Filter to only FRENZ streams
        let frenz_streams: Vec<_> = all_streams
            .into_iter()
            .filter(|s| StreamFilter::is_frenz_stream(&s.info.name))
            .collect();

        // Group streams by device name
        let mut device_map: HashMap<String, DiscoveredFrenzDevice> = HashMap::new();
        let now = std::time::SystemTime::now();

        for stream in frenz_streams {
            let stream_name = &stream.info.name;

            if let (Some(device_name), Some(suffix)) = (
                StreamFilter::extract_frenz_device_name(stream_name),
                StreamFilter::extract_frenz_stream_suffix(stream_name),
            ) {
                let entry = device_map.entry(device_name.clone()).or_insert_with(|| {
                    DiscoveredFrenzDevice {
                        device_name: device_name.clone(),
                        streams: HashMap::new(),
                        discovered_at: now,
                    }
                });

                let category = frenz_stream_category(suffix);

                entry.streams.insert(
                    suffix.to_string(),
                    FrenzStreamInfo {
                        stream_name: stream_name.clone(),
                        suffix: suffix.to_string(),
                        channel_count: stream.info.channel_count,
                        nominal_srate: stream.info.nominal_srate,
                        channel_format: stream.info.channel_format,
                        category,
                        stream_uid: stream.uid.clone(),
                        available: true,
                    },
                );

                debug!(
                    device = "frenz",
                    "Found FRENZ stream: {} ({} ch, {:.1} Hz)",
                    stream_name,
                    stream.info.channel_count,
                    stream.info.nominal_srate
                );
            }
        }

        // Update cache
        {
            let mut devices = self.discovered_devices.write().await;
            *devices = device_map.clone();
        }

        let device_list: Vec<DiscoveredFrenzDevice> = device_map.into_values().collect();
        info!(
            device = "frenz",
            "Discovered {} FRENZ device(s)",
            device_list.len()
        );

        for device in &device_list {
            info!(
                device = "frenz",
                "  - {}: {} streams available",
                device.device_name,
                device.streams.len()
            );
        }

        Ok(device_list)
    }

    /// Get a previously discovered FRENZ device by name
    pub async fn get_device(&self, device_name: &str) -> Option<DiscoveredFrenzDevice> {
        let devices = self.discovered_devices.read().await;
        devices.get(device_name).cloned()
    }

    /// Connect to a single FRENZ LSL stream
    ///
    /// Returns a channel receiver that will emit `FrenzSample` data.
    /// The stream continues until `disconnect_stream` is called.
    pub async fn connect_stream(
        &self,
        device_name: &str,
        stream_suffix: &str,
    ) -> Result<mpsc::Receiver<FrenzSample>, LslError> {
        let _lifecycle = self.lifecycle.lock().await;
        self.connect_stream_inner(device_name, stream_suffix).await
    }

    async fn connect_stream_inner(
        &self,
        device_name: &str,
        stream_suffix: &str,
    ) -> Result<mpsc::Receiver<FrenzSample>, LslError> {
        info!(
            device = "frenz",
            "Connecting to FRENZ stream: {}{}", device_name, stream_suffix
        );

        let task_key = (device_name.to_string(), stream_suffix.to_string());

        // Check if already connected
        {
            let tasks = self.stream_tasks.read().await;
            if tasks.contains_key(&task_key) {
                return Err(LslError::LslLibraryError(format!(
                    "Already connected to stream {}{}",
                    device_name, stream_suffix
                )));
            }
        }

        // Get the device and stream info
        let device = self.get_device(device_name).await.ok_or_else(|| {
            LslError::FrenzDeviceNotFound(format!(
                "Device not found: {}. Run discover_frenz_devices() first.",
                device_name
            ))
        })?;

        let stream_info = device.streams.get(stream_suffix).ok_or_else(|| {
            LslError::FrenzStreamNotAvailable(format!(
                "Stream {} not available for device {}",
                stream_suffix, device_name
            ))
        })?;

        let stream_uid = &stream_info.stream_uid;

        // Find the stream in the resolver cache
        let discovered = self.resolver.get_stream(stream_uid).await.ok_or_else(|| {
            LslError::StreamNotFound(format!("Stream not in cache: {}", stream_uid))
        })?;

        // Create inlet
        let buf_size = buffer_size_for_rate(stream_info.nominal_srate);
        let inlet_config = InletConfig {
            buffer_size: buf_size,
            max_buflen: 360,
            max_chunklen: 0,
            recover: true,
            ..Default::default()
        };

        self.inlet_manager
            .create_inlet(discovered.clone(), Some(inlet_config))
            .await?;

        if let Err(error) = self
            .inlet_manager
            .open_inlet(stream_uid, Duration::from_secs(10))
            .await
        {
            // Creation succeeded, so an open/correction failure still owns a worker.
            if let Err(cleanup) = self.inlet_manager.remove_inlet(stream_uid).await {
                return Err(LslError::LslLibraryError(format!(
                    "{error}; inlet cleanup failed: {cleanup}"
                )));
            }
            return Err(error);
        }

        // Create data channel
        let (tx, rx) = mpsc::channel(buf_size);
        let shutdown = Arc::new(AtomicBool::new(false));

        // Spawn reading task
        let task_tx = tx.clone();
        let task_shutdown = shutdown.clone();
        let task_inlet_manager = self.inlet_manager.clone();
        let task_uid = stream_uid.clone();
        let task_device_name = device_name.to_string();
        let task_suffix = stream_suffix.to_string();

        // Use a longer poll timeout for low-rate streams
        let poll_timeout_ms = if stream_info.nominal_srate < 1.0 {
            1000
        } else if stream_info.nominal_srate < 10.0 {
            500
        } else {
            100
        };

        let handle = tokio::spawn(async move {
            info!(
                device = "frenz",
                "FRENZ stream task started: {}{}", task_device_name, task_suffix
            );

            while !task_shutdown.load(Ordering::Relaxed) {
                match task_inlet_manager
                    .pull_sample(&task_uid, Duration::from_millis(poll_timeout_ms))
                    .await
                {
                    Ok(Some(sample)) => {
                        let frenz_sample = FrenzSample {
                            timestamp: sample.timestamp,
                            stream_suffix: task_suffix.clone(),
                            values: match &sample.data {
                                SampleData::Float64(v) => v.clone(),
                                SampleData::Float32(v) => v.iter().map(|&x| x as f64).collect(),
                                _ => vec![],
                            },
                            string_value: match &sample.data {
                                SampleData::String(v) => v.first().cloned(),
                                _ => None,
                            },
                            data: sample.data,
                        };

                        if task_tx.send(frenz_sample).await.is_err() {
                            debug!(
                                device = "frenz",
                                "Receiver dropped for {}{}, stopping task",
                                task_device_name,
                                task_suffix
                            );
                            break;
                        }
                    }
                    Ok(None) => {
                        // No data available, continue polling
                    }
                    Err(e) => {
                        if !task_shutdown.load(Ordering::Relaxed) {
                            warn!(
                                device = "frenz",
                                "Error pulling sample for {}{}: {}",
                                task_device_name,
                                task_suffix,
                                e
                            );
                        }
                    }
                }
            }

            info!(
                device = "frenz",
                "FRENZ stream task stopped: {}{}", task_device_name, task_suffix
            );
        });

        // Store the task
        {
            let mut tasks = self.stream_tasks.write().await;
            tasks.insert(
                task_key,
                FrenzStreamTask {
                    inlet_uid: stream_uid.clone(),
                    shutdown,
                    _handle: handle,
                },
            );
        }

        info!(
            device = "frenz",
            "Connected to FRENZ stream: {}{}", device_name, stream_suffix
        );
        Ok(rx)
    }

    /// Connect to all discovered streams for a device.
    ///
    /// Returns a map of suffix -> receiver for each connected stream.
    /// If `selected_suffixes` is non-empty, only connects to those streams.
    pub async fn connect_streams(
        &self,
        device_name: &str,
        selected_suffixes: &[String],
    ) -> Result<HashMap<String, mpsc::Receiver<FrenzSample>>, LslError> {
        let _lifecycle = self.lifecycle.lock().await;
        let device = self.get_device(device_name).await.ok_or_else(|| {
            LslError::FrenzDeviceNotFound(format!("Device not found: {}", device_name))
        })?;

        let suffixes: Vec<String> = if selected_suffixes.is_empty() {
            device.streams.keys().cloned().collect()
        } else {
            selected_suffixes.to_vec()
        };

        let mut suffixes = suffixes;
        suffixes.sort();
        suffixes.dedup();
        if suffixes.is_empty() {
            return Err(LslError::FrenzStreamNotAvailable(format!(
                "No streams found for {device_name}"
            )));
        }
        // Validate before allocating anything; never roll back pre-existing readers.
        {
            let tasks = self.stream_tasks.read().await;
            for suffix in &suffixes {
                if !device.streams.contains_key(suffix) {
                    return Err(LslError::FrenzStreamNotAvailable(format!(
                        "Stream {suffix} not available for {device_name}"
                    )));
                }
                if tasks.contains_key(&(device_name.to_string(), suffix.clone())) {
                    return Err(LslError::LslLibraryError(format!(
                        "Already connected to stream {device_name}{suffix}"
                    )));
                }
            }
        }

        let started = std::time::Instant::now();
        // Each inlet owns its native worker. Overlap resolution, opening and the
        // first clock estimate; await every result so failures cannot detach work.
        let results = join_all(
            suffixes
                .iter()
                .map(|suffix| self.connect_stream_inner(device_name, suffix)),
        )
        .await;
        let mut receivers = HashMap::new();
        let mut failures = Vec::new();
        for (suffix, result) in suffixes.iter().zip(results) {
            match result {
                Ok(rx) => {
                    receivers.insert(suffix.clone(), rx);
                }
                Err(error) => failures.push(format!("{suffix}: {error}")),
            }
        }
        if !failures.is_empty() {
            let cleanup = join_all(
                receivers
                    .keys()
                    .map(|suffix| self.disconnect_stream_inner(device_name, suffix)),
            )
            .await;
            for error in cleanup.into_iter().filter_map(Result::err) {
                failures.push(format!("rollback: {error}"));
            }
            return Err(LslError::LslLibraryError(format!(
                "FRENZ connection failed: {}",
                failures.join("; ")
            )));
        }
        info!(
            device = "frenz",
            stream_count = receivers.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Connected all selected streams for {}",
            device_name
        );
        Ok(receivers)
    }

    /// Disconnect from a single FRENZ stream, joining both reader and native inlet.
    pub async fn disconnect_stream(
        &self,
        device_name: &str,
        stream_suffix: &str,
    ) -> Result<(), LslError> {
        let _lifecycle = self.lifecycle.lock().await;
        self.disconnect_stream_inner(device_name, stream_suffix)
            .await
    }

    async fn disconnect_stream_inner(
        &self,
        device_name: &str,
        stream_suffix: &str,
    ) -> Result<(), LslError> {
        let task_key = (device_name.to_string(), stream_suffix.to_string());
        let task = self.stream_tasks.write().await.remove(&task_key);
        if let Some(task) = task {
            task.shutdown.store(true, Ordering::Relaxed);
            // A reader may be blocked on a full consumer channel. Cancellation
            // drops that send future; await the handle rather than detaching it.
            task._handle.abort();
            let reader_result = task._handle.await;
            let cleanup_result = self.inlet_manager.remove_inlet(&task.inlet_uid).await;
            if let Err(error) = reader_result {
                if !error.is_cancelled() {
                    cleanup_result?;
                    return Err(LslError::LslLibraryError(format!(
                        "FRENZ reader failed: {error}"
                    )));
                }
            }
            cleanup_result?;
        }
        info!(
            device = "frenz",
            "Disconnected FRENZ stream: {}{}", device_name, stream_suffix
        );
        Ok(())
    }

    /// Disconnect only this device's owned readers, independently of discovery.
    /// The marker outlet is shared and remains available for other devices.
    pub async fn disconnect_device(&self, device_name: &str) -> Result<(), LslError> {
        let _lifecycle = self.lifecycle.lock().await;
        let started = std::time::Instant::now();
        let task_keys: Vec<(String, String)> = self
            .stream_tasks
            .read()
            .await
            .keys()
            .filter(|(device, _)| device == device_name)
            .cloned()
            .collect();
        let errors = self.disconnect_tasks_inner(&task_keys).await;
        if !errors.is_empty() {
            return Err(LslError::LslLibraryError(format!(
                "FRENZ disconnect failed for {device_name}: {}",
                errors.join("; ")
            )));
        }
        info!(
            device = "frenz",
            device_name,
            stream_count = task_keys.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Disconnected FRENZ device streams"
        );
        Ok(())
    }

    /// Caller holds the lifecycle guard until every native inlet has closed.
    async fn disconnect_tasks_inner(&self, task_keys: &[(String, String)]) -> Vec<String> {
        join_all(
            task_keys
                .iter()
                .map(|(device, suffix)| self.disconnect_stream_inner(device, suffix)),
        )
        .await
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect()
    }

    /// Disconnect all FRENZ streams concurrently and report any cleanup failures.
    pub async fn disconnect_all(&self) -> Result<(), LslError> {
        let _lifecycle = self.lifecycle.lock().await;
        let started = std::time::Instant::now();
        let task_keys: Vec<(String, String)> =
            self.stream_tasks.read().await.keys().cloned().collect();
        let mut errors = self.disconnect_tasks_inner(&task_keys).await;
        let mut outlet_id = self.marker_outlet_id.write().await;
        if let Some(id) = outlet_id.as_ref() {
            match self.outlet_manager.remove_outlet(id).await {
                Ok(()) => {
                    *outlet_id = None;
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        if !errors.is_empty() {
            return Err(LslError::LslLibraryError(format!(
                "FRENZ disconnect failed: {}",
                errors.join("; ")
            )));
        }
        info!(
            device = "frenz",
            stream_count = task_keys.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Disconnected all FRENZ streams"
        );
        Ok(())
    }

    /// Create an LSL marker outlet for sending event markers alongside FRENZ data.
    ///
    /// The outlet is named `HyperStudy_FRENZ_Markers` so LabRecorder can capture
    /// experiment events time-synced with the FRENZ physiological data.
    pub async fn create_marker_outlet(&self) -> Result<(), LslError> {
        {
            let existing = self.marker_outlet_id.read().await;
            if existing.is_some() {
                info!(device = "frenz", "FRENZ marker outlet already exists");
                return Ok(());
            }
        }

        let stream_info = StreamInfo::new(
            "HyperStudy_FRENZ_Markers".to_string(),
            StreamType::Markers,
            1,
            0.0, // Irregular sampling rate
            ChannelFormat::String,
            "hyperstudy-frenz-markers".to_string(),
        );

        let outlet_id = self.outlet_manager.create_outlet(stream_info, None).await?;
        self.outlet_manager.start_outlet(&outlet_id).await?;

        {
            let mut stored_id = self.marker_outlet_id.write().await;
            *stored_id = Some(outlet_id.clone());
        }

        info!(
            device = "frenz",
            "Created FRENZ marker outlet: {}", outlet_id
        );
        Ok(())
    }

    /// Send an event marker to the FRENZ marker outlet
    pub async fn send_marker(&self, marker: &str, timestamp: Option<f64>) -> Result<(), LslError> {
        let outlet_id = {
            let stored = self.marker_outlet_id.read().await;
            stored.clone().ok_or_else(|| {
                LslError::LslLibraryError(
                    "FRENZ marker outlet not created. Call create_marker_outlet() first."
                        .to_string(),
                )
            })?
        };

        let sample = Sample {
            data: SampleData::String(vec![marker.to_string()]),
            timestamp: timestamp.unwrap_or(0.0),
        };

        self.outlet_manager.send_sample(&outlet_id, sample).await?;

        debug!(device = "frenz", "Sent FRENZ marker: {}", marker);
        Ok(())
    }

    /// Get statistics for all FRENZ streams
    pub async fn get_stats(&self) -> serde_json::Value {
        let devices = self.discovered_devices.read().await;
        let tasks = self.stream_tasks.read().await;
        let marker_outlet = self.marker_outlet_id.read().await;

        let device_stats: Vec<serde_json::Value> = devices
            .values()
            .map(|d| {
                let stream_stats: Vec<serde_json::Value> = d
                    .streams
                    .values()
                    .map(|s| {
                        let connected =
                            tasks.contains_key(&(d.device_name.clone(), s.suffix.clone()));
                        serde_json::json!({
                            "suffix": s.suffix,
                            "channel_count": s.channel_count,
                            "nominal_srate": s.nominal_srate,
                            "channel_format": s.channel_format,
                            "category": s.category.to_string(),
                            "connected": connected,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "device_name": d.device_name,
                    "stream_count": d.streams.len(),
                    "streams": stream_stats,
                })
            })
            .collect();

        serde_json::json!({
            "discovered_device_count": devices.len(),
            "active_stream_count": tasks.len(),
            "marker_outlet_active": marker_outlet.is_some(),
            "devices": device_stats,
        })
    }
}

impl std::fmt::Debug for FrenzLslManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrenzLslManager").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::lsl::resolver::StreamFilter;

    fn test_manager() -> FrenzLslManager {
        let time_sync = Arc::new(crate::devices::lsl::sync::TimeSync::new(false));
        FrenzLslManager::new(
            Arc::new(StreamResolver::new(0.1)),
            Arc::new(InletManager::new(time_sync.clone())),
            Arc::new(OutletManager::new(time_sync)),
        )
    }

    #[tokio::test]
    async fn connect_rejects_missing_selected_streams() {
        let manager = test_manager();
        manager.discovered_devices.write().await.insert(
            "test".into(),
            DiscoveredFrenzDevice {
                device_name: "test".into(),
                streams: HashMap::new(),
                discovered_at: std::time::SystemTime::now(),
            },
        );
        let result = manager.connect_streams("test", &["_EEG_raw".into()]).await;
        assert!(
            result.is_err(),
            "A missing selected stream must not appear connected"
        );
    }

    #[tokio::test]
    async fn disconnect_joins_reader_blocked_on_full_channel() {
        let manager = test_manager();
        let (tx, _rx) = mpsc::channel::<u8>(1);
        tx.send(1).await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = started_tx.send(());
            let _ = tx.send(2).await;
        });
        let abort_handle = handle.abort_handle();
        started_rx.await.unwrap();
        manager.stream_tasks.write().await.insert(
            ("test".into(), "_EEG_raw".into()),
            FrenzStreamTask {
                inlet_uid: "test-inlet".into(),
                shutdown: Arc::new(AtomicBool::new(false)),
                _handle: handle,
            },
        );
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            manager.disconnect_stream("test", "_EEG_raw"),
        )
        .await;
        // Also clean up the old implementation's detached task on assertion failure.
        abort_handle.abort();
        assert!(
            result.is_ok(),
            "Disconnect must cancel a blocked channel send"
        );
        assert!(
            abort_handle.is_finished(),
            "Disconnect must join the reader"
        );
    }

    #[tokio::test]
    async fn disconnect_device_preserves_other_device_without_discovery_cache() {
        let manager = test_manager();
        let mut handles = HashMap::new();
        for (device, suffix) in [
            ("target", "_EEG_raw"),
            ("target", "_focus"),
            ("other", "_EEG_raw"),
        ] {
            let handle = tokio::spawn(std::future::pending::<()>());
            handles.insert((device, suffix), handle.abort_handle());
            manager.stream_tasks.write().await.insert(
                (device.into(), suffix.into()),
                FrenzStreamTask {
                    inlet_uid: format!("{device}{suffix}"),
                    shutdown: Arc::new(AtomicBool::new(false)),
                    _handle: handle,
                },
            );
        }
        assert!(manager.discovered_devices.read().await.is_empty());
        manager.disconnect_device("target").await.unwrap();
        let keys: Vec<_> = manager.stream_tasks.read().await.keys().cloned().collect();
        let target_finished = handles[&("target", "_EEG_raw")].is_finished()
            && handles[&("target", "_focus")].is_finished();
        let other_finished = handles[&("other", "_EEG_raw")].is_finished();
        manager.disconnect_all().await.unwrap();
        assert_eq!(keys, vec![("other".into(), "_EEG_raw".into())]);
        assert!(target_finished, "Both target readers must be joined");
        assert!(
            !other_finished,
            "Unrelated device reader must remain active"
        );
    }

    #[test]
    fn websocket_payload_preserves_large_integers() {
        use base64::Engine;
        let sample = FrenzSample {
            data: SampleData::Int64(vec![i64::MIN, i64::MAX]),
            timestamp: 1.,
            stream_suffix: "_EEG_firmware".into(),
            values: vec![],
            string_value: None,
        };
        let payload = sample.wire_data();
        assert_eq!(payload["format"], "int64");
        assert_eq!(payload["encoding"], "base64-le");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload["values"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            bytes,
            [i64::MIN.to_le_bytes(), i64::MAX.to_le_bytes()].concat()
        );
    }

    #[test]
    fn test_is_frenz_stream() {
        // All 16 known suffixes should match
        assert!(StreamFilter::is_frenz_stream("DEVICE123_EEG_raw"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_EEG_filtered"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_EOG_filtered"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_EMG_filtered"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_PPG_raw"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_IMU_raw"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_POSTURE"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_poas"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_sleep_stage"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_focus"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_signal_quality"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_alpha"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_beta"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_theta"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_gamma"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_delta"));

        // Case-insensitive matching
        assert!(StreamFilter::is_frenz_stream("DEVICE123_eeg_RAW"));
        assert!(StreamFilter::is_frenz_stream("DEVICE123_POAS"));

        // Non-FRENZ streams should not match
        assert!(!StreamFilter::is_frenz_stream("MyNeon_Neon Gaze"));
        assert!(!StreamFilter::is_frenz_stream("SomeOtherStream"));
        assert!(!StreamFilter::is_frenz_stream("Random_EEG")); // No underscore before suffix
    }

    #[test]
    fn test_extract_frenz_device_name() {
        assert_eq!(
            StreamFilter::extract_frenz_device_name("FRENZ_ABC123_EEG_raw"),
            Some("FRENZ_ABC123".to_string())
        );
        assert_eq!(
            StreamFilter::extract_frenz_device_name("MyDevice_focus"),
            Some("MyDevice".to_string())
        );
        assert_eq!(
            StreamFilter::extract_frenz_device_name("Dev_POSTURE"),
            Some("Dev".to_string())
        );
        assert_eq!(
            StreamFilter::extract_frenz_device_name("MyNeon_Neon Gaze"),
            None
        );
    }

    #[test]
    fn test_extract_frenz_stream_suffix() {
        assert_eq!(
            StreamFilter::extract_frenz_stream_suffix("FRENZ_ABC_EEG_raw"),
            Some("_EEG_raw")
        );
        assert_eq!(
            StreamFilter::extract_frenz_stream_suffix("Dev_poas"),
            Some("_poas")
        );
        assert_eq!(
            StreamFilter::extract_frenz_stream_suffix("Dev_POSTURE"),
            Some("_POSTURE")
        );
        // Case-insensitive should return canonical case
        assert_eq!(
            StreamFilter::extract_frenz_stream_suffix("Dev_POAS"),
            Some("_poas")
        );
        assert_eq!(
            StreamFilter::extract_frenz_stream_suffix("Not_A_Frenz_Stream"),
            None
        );
    }

    #[test]
    fn test_buffer_size_selection() {
        // High-rate streams (125Hz)
        assert_eq!(buffer_size_for_rate(125.0), 1000);
        // Medium-rate streams (50Hz)
        assert_eq!(buffer_size_for_rate(50.0), 500);
        // Low-rate streams (25Hz)
        assert_eq!(buffer_size_for_rate(25.0), 500);
        // Very low rate (0.5Hz)
        assert_eq!(buffer_size_for_rate(0.5), 50);
        // Sub-Hz (0.2Hz)
        assert_eq!(buffer_size_for_rate(0.2), 50);
    }

    #[test]
    fn test_frenz_manager_creation() {
        use crate::devices::lsl::inlet::InletManager;
        use crate::devices::lsl::outlet::OutletManager;
        use crate::devices::lsl::resolver::StreamResolver;
        use crate::devices::lsl::sync::TimeSync;

        let time_sync = Arc::new(TimeSync::new(false));
        let resolver = Arc::new(StreamResolver::new(1.0));
        let inlet_manager = Arc::new(InletManager::new(time_sync.clone()));
        let outlet_manager = Arc::new(OutletManager::new(time_sync));

        let manager = FrenzLslManager::new(resolver, inlet_manager, outlet_manager);
        // Verify creation doesn't panic
        let _ = format!("{:?}", manager);
    }

    #[tokio::test]
    async fn test_frenz_discovery_no_streams() {
        use crate::devices::lsl::inlet::InletManager;
        use crate::devices::lsl::outlet::OutletManager;
        use crate::devices::lsl::resolver::StreamResolver;
        use crate::devices::lsl::sync::TimeSync;

        let time_sync = Arc::new(TimeSync::new(false));
        let resolver = Arc::new(StreamResolver::new(0.5));
        let inlet_manager = Arc::new(InletManager::new(time_sync.clone()));
        let outlet_manager = Arc::new(OutletManager::new(time_sync));

        let manager = FrenzLslManager::new(resolver, inlet_manager, outlet_manager);

        // Discovery should succeed even with no FRENZ streams on network
        let devices = manager.discover_frenz_devices().await.unwrap();
        // We can't guarantee any FRENZ devices are on the network, just verify no panic
        let stats = manager.get_stats().await;
        assert_eq!(
            stats["discovered_device_count"].as_u64().unwrap_or(0),
            devices.len() as u64
        );
    }
}
