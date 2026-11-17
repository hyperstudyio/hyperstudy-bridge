use super::types::{ChannelFormat, LslError, StreamInfo, StreamType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// Import real LSL library
use lsl;

/// Maximum number of discovered streams to cache (prevents unbounded HashMap growth)
const MAX_DISCOVERED_STREAMS: usize = 100;

/// Stream discovery and resolution service
#[derive(Debug)]
pub struct StreamResolver {
    /// Discovered streams cache
    discovered_streams: RwLock<HashMap<String, DiscoveredStream>>,
    /// Discovery timeout
    timeout_duration: Duration,
}

/// Information about a discovered LSL stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredStream {
    pub info: StreamInfo,
    pub discovered_at: std::time::SystemTime,
    pub last_seen: std::time::SystemTime,
    pub available: bool,
    pub uid: String,
    pub session_id: String,
    pub data_loss: f64,
}

/// Stream-name classification for the Neon and FRENZ streams Bridge consumes
pub struct StreamFilter;

impl StreamFilter {
    /// Check if a stream name indicates a Neon gaze stream
    pub fn is_neon_gaze_stream(stream_name: &str) -> bool {
        stream_name.ends_with("_Neon Gaze")
    }

    /// Check if a stream name indicates a Neon events stream
    pub fn is_neon_events_stream(stream_name: &str) -> bool {
        stream_name.ends_with("_Neon Events")
    }

    /// Extract device name from Neon stream name
    ///
    /// For example, "MyNeon_Neon Gaze" -> "MyNeon"
    pub fn extract_neon_device_name(stream_name: &str) -> Option<String> {
        if let Some(pos) = stream_name.find("_Neon Gaze") {
            Some(stream_name[..pos].to_string())
        } else if let Some(pos) = stream_name.find("_Neon Events") {
            Some(stream_name[..pos].to_string())
        } else {
            None
        }
    }

    // ====================================================================
    // FRENZ Brainband stream filters
    // ====================================================================

    /// Check if a stream name matches a known FRENZ stream suffix.
    ///
    /// FRENZ streams are named `{DEVICE_ID}_{suffix}` by the Python LSL bridge.
    /// Naming is case-inconsistent (e.g., `_EEG_raw` vs `_poas` vs `_POSTURE`),
    /// so we do case-insensitive matching.
    pub fn is_frenz_stream(stream_name: &str) -> bool {
        let lower = stream_name.to_lowercase();
        super::types::FRENZ_STREAM_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(&suffix.to_lowercase()))
    }

    /// Extract the device name prefix from a FRENZ stream name.
    ///
    /// For example, "FRENZ_ABC123_EEG_raw" -> "FRENZ_ABC123"
    pub fn extract_frenz_device_name(stream_name: &str) -> Option<String> {
        let lower = stream_name.to_lowercase();
        for suffix in super::types::FRENZ_STREAM_SUFFIXES {
            let suffix_lower = suffix.to_lowercase();
            if lower.ends_with(&suffix_lower) {
                let prefix_len = stream_name.len() - suffix.len();
                return Some(stream_name[..prefix_len].to_string());
            }
        }
        None
    }

    /// Extract the stream suffix from a FRENZ stream name.
    ///
    /// For example, "FRENZ_ABC123_EEG_raw" -> "_EEG_raw"
    /// Returns the canonical suffix (from the constant array) to normalize case.
    pub fn extract_frenz_stream_suffix(stream_name: &str) -> Option<&'static str> {
        let lower = stream_name.to_lowercase();
        for suffix in super::types::FRENZ_STREAM_SUFFIXES {
            let suffix_lower = suffix.to_lowercase();
            if lower.ends_with(&suffix_lower) {
                return Some(suffix);
            }
        }
        None
    }
}

/// Internal struct to hold data extracted from lsl::StreamInfo inside spawn_blocking.
/// This struct contains only Send types (String, primitives) so it can cross thread boundaries.
#[derive(Debug)]
struct ExtractedStreamData {
    name: String,
    stream_type: String,
    channel_count: i32,
    nominal_srate: f64,
    channel_format: lsl::ChannelFormat,
    source_id: String,
    hostname: String,
    uid: String,
    session_id: String,
}

impl StreamResolver {
    /// Create a new stream resolver
    pub fn new(timeout_seconds: f64) -> Self {
        Self {
            discovered_streams: RwLock::new(HashMap::new()),
            timeout_duration: Duration::from_secs_f64(timeout_seconds),
        }
    }

    /// Perform one-time stream discovery using real LSL library
    pub async fn discover_streams(&self) -> Result<Vec<DiscoveredStream>, LslError> {
        info!(
            device = "lsl",
            "Performing one-time stream discovery (timeout: {:?})", self.timeout_duration
        );

        let discovery_start = Instant::now();
        let timeout_secs = self.timeout_duration.as_secs_f64();

        // Use spawn_blocking because lsl::resolve_streams is a blocking FFI call.
        // CRITICAL: Extract ALL data from lsl::StreamInfo INSIDE the closure because
        // lsl::StreamInfo contains Rc internally (not Send) and cannot cross thread boundaries.
        let extracted_streams = tokio::task::spawn_blocking(move || {
            let lsl_streams = lsl::resolve_streams(timeout_secs)?;

            // Extract all data inside this closure - return only Send types
            let extracted: Vec<ExtractedStreamData> = lsl_streams
                .into_iter()
                .map(|info| ExtractedStreamData {
                    name: info.stream_name(),
                    stream_type: info.stream_type(),
                    channel_count: info.channel_count(),
                    nominal_srate: info.nominal_srate(),
                    channel_format: info.channel_format(),
                    source_id: info.source_id(),
                    hostname: info.hostname(),
                    uid: info.uid(),
                    session_id: info.session_id(),
                })
                .collect();

            Ok::<_, lsl::Error>(extracted)
        })
        .await
        .map_err(|e| LslError::LslLibraryError(format!("Task join error: {}", e)))?
        .map_err(|e| LslError::LslLibraryError(format!("LSL resolve error: {:?}", e)))?;

        let now = std::time::SystemTime::now();
        let mut discovered = Vec::new();

        for extracted in extracted_streams {
            // Map LSL channel format to our ChannelFormat
            let channel_format = match extracted.channel_format {
                lsl::ChannelFormat::Float32 => ChannelFormat::Float32,
                lsl::ChannelFormat::Double64 => ChannelFormat::Float64,
                lsl::ChannelFormat::String => ChannelFormat::String,
                lsl::ChannelFormat::Int8 => ChannelFormat::Int8,
                lsl::ChannelFormat::Int16 => ChannelFormat::Int16,
                lsl::ChannelFormat::Int32 => ChannelFormat::Int32,
                lsl::ChannelFormat::Int64 => ChannelFormat::Int64,
                _ => ChannelFormat::Float32, // Default fallback
            };

            // Map stream type string to our StreamType enum
            let stream_type = match extracted.stream_type.to_lowercase().as_str() {
                "markers" | "marker" => StreamType::Markers,
                "fnirs" | "nirs" => StreamType::FNIRS,
                "gaze" | "eyetracking" => StreamType::Gaze,
                // EEG, EMG, ECG, and other biosignals map to Biosignals
                "eeg" | "emg" | "ecg" | "biosignals" | "physio" | "physiological" => {
                    StreamType::Biosignals
                }
                // Video, Audio, and others map to Generic
                _ => StreamType::Generic,
            };

            let stream_info = StreamInfo {
                name: extracted.name,
                stream_type,
                channel_count: extracted.channel_count as u32,
                channel_format,
                nominal_srate: extracted.nominal_srate,
                source_id: extracted.source_id,
                hostname: extracted.hostname,
                metadata: std::collections::HashMap::new(),
            };

            discovered.push(DiscoveredStream {
                info: stream_info,
                discovered_at: now,
                last_seen: now,
                available: true,
                uid: extracted.uid,
                session_id: extracted.session_id,
                data_loss: 0.0,
            });
        }

        let discovery_time = discovery_start.elapsed();
        info!(
            device = "lsl",
            "Discovery completed in {:?}, found {} streams",
            discovery_time,
            discovered.len()
        );

        // Update cache with size limit enforcement
        let mut cache = self.discovered_streams.write().await;
        for stream in &discovered {
            cache.insert(stream.uid.clone(), stream.clone());
        }

        // Enforce maximum cache size to prevent unbounded memory growth
        if cache.len() > MAX_DISCOVERED_STREAMS {
            // Remove oldest entries by discovered_at timestamp
            let mut entries: Vec<_> = cache
                .iter()
                .map(|(uid, stream)| (uid.clone(), stream.discovered_at))
                .collect();
            entries.sort_by(|a, b| a.1.cmp(&b.1));

            let to_remove = cache.len() - MAX_DISCOVERED_STREAMS;
            for (uid, _) in entries.into_iter().take(to_remove) {
                cache.remove(&uid);
                debug!(device = "lsl", "Evicted old stream from cache: {}", uid);
            }

            warn!(
                device = "lsl",
                "Stream cache exceeded limit, evicted {} oldest entries", to_remove
            );
        }

        Ok(discovered)
    }

    /// Get stream by UID
    pub async fn get_stream(&self, uid: &str) -> Option<DiscoveredStream> {
        let cache = self.discovered_streams.read().await;
        cache.get(uid).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_resolver_creation() {
        let resolver = StreamResolver::new(5.0);
        assert_eq!(resolver.timeout_duration, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_stream_discovery() {
        // Use a short timeout since we're testing the API, not waiting for streams
        let resolver = StreamResolver::new(0.5);
        let streams = resolver.discover_streams().await.unwrap();

        // Real LSL discovery may or may not find streams depending on environment
        // Just verify discovery completes without error and caching works
        for stream in &streams {
            assert!(resolver.get_stream(&stream.uid).await.is_some());
        }
    }
}
