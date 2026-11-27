//! FRENZ through openfrenz: Bridge connects to the band itself, without the vendor SDK helper.
//!
//! One connection does three things:
//! - publishes the band's streams and the outputs of openfrenz's built-in models as LSL outlets, named and
//!   identified the way HyperStudy recognises FRENZ streams (`<band>_<STREAM>`, source id
//!   `frenz-<band>-<session uuid>_<STREAM>`), so study recordings work as they do with the SDK helper;
//! - keeps the last stretch of every stream and model output for the Bridge window to draw;
//! - reports its state on the `frenz_native_status` event.
//!
//! The band accepts one connection, so this source and the SDK helper (`lsl::frenz_process`) must never run
//! together; the commands that start each refuse while the other is active.
//!
//! liblsl handles are not `Send`, so every outlet lives on one dedicated thread fed through a channel.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use frenz::{Band, ConnectOptions, ReplayOptions};
use frenz_model::streams::{self, Chunk, End, Streams};
use frenz_model::{Live, Model, Pipeline};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// The openfrenz models run on every connection, and the LSL stream each one becomes.
pub const MODELS: [(&str, &str); 5] = [
    ("eog", "_EOG_openfrenz"),
    ("brow-emg", "_BROW_EMG_openfrenz"),
    ("heart", "_HEART_openfrenz"),
    ("head", "_HEAD_openfrenz"),
    ("eeg-power", "_EEG_POWER_openfrenz"),
];

/// The band's streams that are published, and the LSL stream each one becomes.
pub const STREAMS: [(&str, &str); 7] = [
    ("eeg", "_EEG"),
    ("accel", "_ACCEL"),
    ("gyro", "_GYRO"),
    ("ppg", "_PPG"),
    ("heart_rate", "_HR"),
    ("spo2", "_SPO2"),
    ("contact", "_CONTACT"),
];

/// Seconds of each stream kept for drawing, and how long a scan listens.
const KEEP_SECONDS: f64 = 30.0;
const SCAN: Duration = Duration::from_secs(6);
/// Model outputs kept for drawing.
const KEEP_OUTPUTS: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeState {
    Stopped,
    Connecting,
    Streaming,
    Error,
}

/// What the Bridge window shows about the connection.
#[derive(Debug, Clone, Serialize)]
pub struct NativeStatus {
    pub state: NativeState,
    pub message: Option<String>,
    pub device: Option<String>,
    /// Names of the LSL streams being published.
    pub streams: Vec<String>,
    /// Samples published so far, by LSL stream name.
    pub samples: BTreeMap<String, u64>,
    /// Models asked for that could not be loaded, each with why; the rest run without them.
    pub model_errors: Vec<String>,
    /// Models running in place of another for this connection, by the name they run under (the original's):
    /// the folder name of the pipeline standing in. The original comes back at the next connection.
    pub stand_ins: BTreeMap<String, String>,
}

impl NativeStatus {
    fn stopped() -> Self {
        Self {
            state: NativeState::Stopped,
            message: None,
            device: None,
            streams: Vec::new(),
            samples: BTreeMap::new(),
            model_errors: Vec::new(),
            stand_ins: BTreeMap::new(),
        }
    }
}

/// A band seen in a scan.
#[derive(Debug, Clone, Serialize)]
pub struct NativeBand {
    pub name: String,
    pub rssi: Option<i16>,
    /// False when two bands advertise the same name: connecting by name would be a guess.
    pub selectable: bool,
}

/// A stretch of one stream or model output for drawing: `values[channel][sample]`.
#[derive(Debug, Clone, Serialize)]
pub struct Trace {
    pub name: String,
    pub channels: Vec<String>,
    /// Unix seconds of each sample.
    pub t: Vec<f64>,
    pub values: Vec<Vec<f64>>,
}

/// What the publishing thread is asked to do.
enum ToLsl {
    Open {
        suffix: String,
        channels: Vec<String>,
        rate_hz: f64,
        source: String,
        reply: mpsc::Sender<Result<String, String>>,
    },
    /// Samples for one stream: Unix time and one row of channel values each.
    Push {
        suffix: String,
        stamps: Vec<f64>,
        rows: Vec<Vec<f64>>,
    },
    /// Closes one stream's outlet: a model replaced while connected.
    CloseOne {
        suffix: String,
    },
    Close,
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

/// What to subtract from a Unix time to put it on this computer's LSL clock, measured once per connection: the
/// wall clock read between two reads of the LSL clock. The study recorder stores its own `host_wall_lsl`
/// samples, so the mapping can be checked and refitted afterwards.
fn unix_minus_lsl() -> f64 {
    let before = lsl::local_clock();
    let wall = unix_now();
    let after = lsl::local_clock();
    wall - (before + after) / 2.0
}

/// Owns every outlet of one connection until it is told to close or its sender is dropped.
fn publish(device: String, session: String, commands: mpsc::Receiver<ToLsl>) {
    use lsl::ExPushable;
    let offset = unix_minus_lsl();
    let mut outlets: BTreeMap<String, lsl::StreamOutlet> = BTreeMap::new();
    let open = |suffix: &str,
                channels: &[String],
                rate_hz: f64,
                source: &str|
     -> Result<(String, lsl::StreamOutlet), String> {
        let name = format!("{device}{suffix}");
        let mut info = lsl::StreamInfo::new(
            &name,
            "Biosignals",
            channels.len() as u32,
            rate_hz,
            lsl::ChannelFormat::Double64,
            &format!("frenz-{device}-{session}{suffix}"),
        )
        .map_err(|e| format!("{name}: {e}"))?;
        let mut desc = info.desc();
        for (key, value) in [
            ("schema_version", "openfrenz-1"),
            ("manufacturer", "Earable"),
            ("publisher", "HyperStudy Bridge (openfrenz)"),
            ("device_id", device.as_str()),
            ("session_id", session.as_str()),
            ("openfrenz_source", source),
            // Each sample's time is openfrenz's estimate of when it was taken, in Unix seconds, moved to the
            // LSL clock with one offset measured when the connection opened.
            ("timestamp_policy", "source_unix"),
            ("source_unix_to_lsl_offset", &format!("{:.9}", -offset)),
            (
                "values",
                "raw device counts; model outputs in the units openfrenz documents",
            ),
            ("repeated_packets", "left out"),
        ] {
            desc.append_child_value(key, value);
        }
        let mut list = desc.append_child("channels");
        for label in channels {
            list.append_child("channel")
                .append_child_value("label", label);
        }
        let outlet = lsl::StreamOutlet::new(&info, 0, 360).map_err(|e| format!("{name}: {e}"))?;
        Ok((name, outlet))
    };
    while let Ok(command) = commands.recv() {
        match command {
            ToLsl::Open {
                suffix,
                channels,
                rate_hz,
                source,
                reply,
            } => {
                let made = open(&suffix, &channels, rate_hz, &source).map(|(name, outlet)| {
                    outlets.insert(suffix, outlet);
                    name
                });
                let _ = reply.send(made);
            }
            ToLsl::Push {
                suffix,
                stamps,
                rows,
            } => {
                if let Some(outlet) = outlets.get(&suffix) {
                    let stamps: Vec<f64> = stamps.iter().map(|t| t - offset).collect();
                    if let Err(e) = outlet.push_chunk_stamped_ex(&rows, &stamps, true) {
                        warn!(device = "frenz", "publishing {suffix}: {e}");
                    }
                }
            }
            ToLsl::CloseOne { suffix } => {
                outlets.remove(&suffix);
            }
            ToLsl::Close => break,
        }
    }
}

/// The samples of a chunk worth passing on: repeated and frozen packets are left out, as openfrenz's own
/// model runner does.
fn rows_of(chunk: &Chunk) -> (Vec<f64>, Vec<Vec<f64>>) {
    let width = chunk.channels.len();
    let kept = chunk.rows.iter().filter(|r| !(r.duplicate || r.held));
    kept.map(|r| {
        (
            r.t,
            r.values.iter().take(width).map(|v| f64::from(*v)).collect(),
        )
    })
    .unzip()
}

/// Recent outputs of one model: time and values.
type Outputs = VecDeque<(f64, Vec<f64>)>;

/// What is shared between the tasks of one connection and the commands that read it.
struct Shared {
    status: StdMutex<NativeStatus>,
    outputs: StdMutex<BTreeMap<String, Outputs>>,
    output_names: StdMutex<BTreeMap<String, Vec<String>>>,
    /// The models running, in order: the name they are drawn under and their LSL suffix.
    models: StdMutex<Vec<(String, String)>>,
    /// The folder each running model was loaded from, by name: what the pipeline editor opens.
    dirs: StdMutex<BTreeMap<String, PathBuf>>,
    /// For a model with a copy standing in for it, the folder of the original, to revert to.
    originals: StdMutex<BTreeMap<String, PathBuf>>,
    /// The editor's pipeline under trial: its output names and recent outputs.
    preview: StdMutex<Option<(Vec<String>, Outputs)>>,
    app: Option<AppHandle>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut NativeStatus)) {
        let snapshot = {
            let Ok(mut status) = self.status.lock() else {
                return;
            };
            change(&mut status);
            status.clone()
        };
        if let Some(app) = &self.app {
            if let Err(e) = app.emit("frenz_native_status", &snapshot) {
                warn!(
                    device = "frenz",
                    "could not report the openfrenz status: {e}"
                );
            }
        }
    }

    fn count(&self, stream: &str, samples: usize) {
        if let Ok(mut status) = self.status.lock() {
            *status.samples.entry(stream.to_owned()).or_default() += samples as u64;
        }
    }

    fn fail(&self, message: String) {
        error!(device = "frenz", "openfrenz: {message}");
        self.update(|s| {
            s.state = NativeState::Error;
            s.message = Some(message);
        });
    }
}

struct Session {
    band: Option<Band>,
    /// Whether the band retries a lost link (false for a replay).
    reconnects: bool,
    streams: Arc<Streams>,
    tasks: Vec<JoinHandle<()>>,
    /// Each model's task, by the name it is drawn under, so that one can be replaced while connected.
    models: BTreeMap<String, JoinHandle<()>>,
    /// The pipeline being tried in the editor, if any: drawn, never published.
    preview: Option<JoinHandle<()>>,
    lsl: mpsc::Sender<ToLsl>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The openfrenz connection to a band. One per app.
pub struct FrenzNative {
    session: Mutex<Option<Session>>,
    shared: StdMutex<Arc<Shared>>,
}

impl Default for FrenzNative {
    fn default() -> Self {
        Self::new()
    }
}

impl FrenzNative {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            shared: StdMutex::new(Arc::new(Shared {
                status: StdMutex::new(NativeStatus::stopped()),
                outputs: StdMutex::new(BTreeMap::new()),
                output_names: StdMutex::new(BTreeMap::new()),
                models: StdMutex::new(Vec::new()),
                dirs: StdMutex::new(BTreeMap::new()),
                originals: StdMutex::new(BTreeMap::new()),
                preview: StdMutex::new(None),
                app: None,
            })),
        }
    }

    fn shared(&self) -> Arc<Shared> {
        self.shared
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn status(&self) -> NativeStatus {
        self.shared()
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| NativeStatus::stopped())
    }

    /// Whether a band is connected or being connected.
    pub fn active(&self) -> bool {
        matches!(
            self.status().state,
            NativeState::Connecting | NativeState::Streaming
        )
    }

    /// Bands advertising nearby. Not possible while connected: the connected band does not advertise.
    pub async fn scan(&self) -> Result<Vec<NativeBand>, String> {
        let found = frenz::scan(SCAN).await.map_err(|e| e.to_string())?;
        Ok(found
            .iter()
            .map(|band| NativeBand {
                name: band.name.clone(),
                rssi: band.rssi,
                selectable: found.iter().filter(|other| other.name == band.name).count() == 1,
            })
            .collect())
    }

    /// Connects to the band called `device` and starts publishing, running openfrenz's built-in models and the
    /// `extra` ones: Hugging Face repositories (`owner/name`, `owner/name@revision`) read with `token`, or
    /// folders. An extra model that cannot be loaded is reported in the status and left out.
    pub async fn start(
        &self,
        device: &str,
        extra: &[String],
        instead: &[(String, String)],
        token: Option<String>,
        app: Option<AppHandle>,
    ) -> Result<(), String> {
        self.start_with(device, extra, instead, token, app, None)
            .await
    }

    /// Plays a recording back as if it were the band `device`, at its recorded pace. For trying the app
    /// without a band.
    pub async fn start_replay(
        &self,
        device: &str,
        recording: &std::path::Path,
        extra: &[String],
        instead: &[(String, String)],
        token: Option<String>,
        app: Option<AppHandle>,
    ) -> Result<(), String> {
        self.start_with(device, extra, instead, token, app, Some(recording))
            .await
    }

    /// `instead`: models to run in place of others from the start, as `(name it replaces, folder)`: an edited
    /// copy of a built-in, kept in the configuration. The original is remembered, to revert to.
    async fn start_with(
        &self,
        device: &str,
        extra: &[String],
        instead: &[(String, String)],
        token: Option<String>,
        app: Option<AppHandle>,
        replay: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let plain = !device.is_empty()
            && device
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !plain {
            return Err("The band's name may hold letters, digits, - and _ only.".into());
        }
        let mut session = self.session.lock().await;
        if session.is_some() {
            return Err("openfrenz is already connected. Disconnect first.".into());
        }
        let shared = Arc::new(Shared {
            status: StdMutex::new(NativeStatus {
                state: NativeState::Connecting,
                device: Some(device.to_owned()),
                ..NativeStatus::stopped()
            }),
            outputs: StdMutex::new(BTreeMap::new()),
            output_names: StdMutex::new(BTreeMap::new()),
            models: StdMutex::new(Vec::new()),
            dirs: StdMutex::new(BTreeMap::new()),
            originals: StdMutex::new(BTreeMap::new()),
            preview: StdMutex::new(None),
            app,
        });
        if let Ok(mut current) = self.shared.lock() {
            *current = shared.clone();
        }
        shared.update(|_| {});
        // Fetched before the band is connected: a download can take a while, and needs no band.
        let (mut models, mut errors) = load_models(extra.to_vec(), token).await;
        let swapped = swap_in(&mut models, instead, &mut errors).await;
        if let Ok(mut originals) = shared.originals.lock() {
            for (name, original, _) in &swapped {
                originals.insert(name.clone(), original.clone());
            }
        }
        shared.update(|s| {
            s.model_errors = errors;
            s.stand_ins = swapped
                .into_iter()
                .map(|(name, _, copy)| (name, copy))
                .collect();
        });
        match open(device, replay, models, shared.clone()).await {
            Ok(opened) => {
                *session = Some(opened);
                Ok(())
            }
            Err(message) => {
                shared.fail(message.clone());
                Err(message)
            }
        }
    }

    /// What a HyperStudy status request for FRENZ is answered with while this source is streaming, in the shape
    /// the SDK helper's streams are reported in. `None` when it is not streaming.
    pub fn stats(&self) -> Option<serde_json::Value> {
        let status = self.status();
        if status.state != NativeState::Streaming {
            return None;
        }
        let shared = self.shared();
        let outputs = shared.output_names.lock().ok()?;
        let running = shared.models.lock().ok()?.clone();
        let raw = STREAMS.iter().map(|(stream, suffix)| {
            let channels = streams::channels(stream).map_or(0, <[&str]>::len);
            (
                *suffix,
                channels,
                streams::rate_hz(stream).unwrap_or(0.0),
                "openfrenz",
            )
        });
        let models = running.iter().map(|(model, suffix)| {
            (
                suffix.as_str(),
                outputs.get(model).map_or(0, Vec::len),
                0.0,
                "openfrenz model",
            )
        });
        let streams: Vec<serde_json::Value> = raw
            .chain(models)
            .map(|(suffix, channels, rate, category)| {
                serde_json::json!({
                    "suffix": suffix,
                    "channel_count": channels,
                    "nominal_srate": rate,
                    "channel_format": "double64",
                    "category": category,
                    "connected": true,
                })
            })
            .collect();
        Some(serde_json::json!({
            "source": "openfrenz",
            "discovered_device_count": 1,
            "active_stream_count": streams.len(),
            "devices": [{
                "device_name": status.device,
                "stream_count": streams.len(),
                "streams": streams,
            }],
        }))
    }

    /// Stops publishing and disconnects. Safe to call when nothing is connected.
    pub async fn stop(&self) {
        // Held to the end: a connection started while this one is still closing would otherwise be marked stopped
        // below while it holds the band.
        let mut slot = self.session.lock().await;
        let Some(mut session) = slot.take() else {
            return;
        };
        let models = std::mem::take(&mut session.models).into_values();
        for task in session
            .tasks
            .drain(..)
            .chain(models)
            .chain(session.preview.take())
        {
            task.abort();
        }
        let _ = session.lsl.send(ToLsl::Close);
        if let Some(thread) = session.thread.take() {
            // Closing outlets can take a moment; never on the async threads.
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
        if let Some(band) = session.band.take() {
            if let Err(e) = band.stop().await {
                warn!(device = "frenz", "stopping the band: {e}");
            }
        }
        self.shared().update(|s| *s = NativeStatus::stopped());
        info!(device = "frenz", "openfrenz disconnected");
    }

    /// Runs `pipeline` on the connected band for the pipeline editor, replacing any pipeline already under trial.
    /// Its outputs are drawn as the trace `preview` and never published. Returns their names.
    pub async fn preview(&self, pipeline: Pipeline) -> Result<Vec<String>, String> {
        let names = pipeline.spec().outputs.clone();
        let mut session = self.session.lock().await;
        let Some(session) = session.as_mut() else {
            return Err("Connect the band through openfrenz to try a pipeline on it.".into());
        };
        let Some(band) = session.band.as_ref() else {
            return Err("The band is not connected.".into());
        };
        let mut live = Live::new(pipeline, band, session.reconnects).map_err(|e| e.to_string())?;
        if let Some(old) = session.preview.take() {
            old.abort();
        }
        let shared = self.shared();
        if let Ok(mut preview) = shared.preview.lock() {
            *preview = Some((names.clone(), VecDeque::new()));
        }
        session.preview = Some(tokio::spawn(async move {
            while let Ok(Some(output)) = live.next().await {
                if let Ok(mut preview) = shared.preview.lock() {
                    if let Some((_, kept)) = preview.as_mut() {
                        kept.push_back((output.t, output.values));
                        while kept.len() > KEEP_OUTPUTS {
                            kept.pop_front();
                        }
                    }
                }
            }
        }));
        Ok(names)
    }

    /// Stops the pipeline under trial.
    pub async fn stop_preview(&self) {
        if let Some(session) = self.session.lock().await.as_mut() {
            if let Some(task) = session.preview.take() {
                task.abort();
            }
        }
        if let Ok(mut preview) = self.shared().preview.lock() {
            *preview = None;
        }
    }

    /// The folder the running model `name` was loaded from.
    pub fn model_dir(&self, name: &str) -> Option<PathBuf> {
        self.shared().dirs.lock().ok()?.get(name).cloned()
    }

    /// The folders of every running model.
    pub fn model_dirs(&self) -> Vec<PathBuf> {
        self.shared()
            .dirs
            .lock()
            .map(|d| d.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the model running as `name` stands in for another this connection.
    pub fn stands_in(&self, name: &str) -> bool {
        self.status().stand_ins.contains_key(name)
    }

    /// Starts the model in `dir` on the connected band, published and drawn like the others. With `instead_of`,
    /// it runs in place of that running model (a built-in, say) until the band disconnects: under its name, on
    /// its stream and in its chart. Otherwise a model already running from the same folder is replaced, keeping
    /// its name and stream. Returns the name it runs under.
    pub async fn run_model(
        &self,
        dir: PathBuf,
        instead_of: Option<&str>,
    ) -> Result<String, String> {
        let shared = self.shared();
        let same = |a: &Path, b: &Path| {
            a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
        };
        let replacing = match instead_of {
            Some(name) => {
                let running = shared
                    .models
                    .lock()
                    .map(|m| m.iter().any(|(n, _)| n == name));
                if !running.unwrap_or(false) {
                    return Err(format!(
                        "{name} is not running, so nothing can run in its place."
                    ));
                }
                Some(name.to_owned())
            }
            None => shared.dirs.lock().ok().and_then(|dirs| {
                dirs.iter()
                    .find(|(_, d)| same(d, &dir))
                    .map(|(name, _)| name.clone())
            }),
        };
        // Standing in for another model: asked to, or already doing so and changed again.
        let stand_in = instead_of.is_some()
            || replacing
                .as_deref()
                .is_some_and(|name| self.stands_in(name));
        self.start_in_place(dir, replacing, stand_in).await
    }

    /// Puts back the model a copy stands in for (see [`FrenzNative::run_model`]), on the same stream.
    pub async fn revert_model(&self, name: &str) -> Result<(), String> {
        let original = self
            .shared()
            .originals
            .lock()
            .ok()
            .and_then(|o| o.get(name).cloned())
            .ok_or_else(|| format!("{name} is not an edited copy."))?;
        self.start_in_place(original, Some(name.to_owned()), false)
            .await
            .map(|_| ())
    }

    /// Starts the model in `dir`, in place of the running model `replacing` if any (its task stopped, its outlet
    /// closed and opened again under the same name), as a stand-in for it or not.
    async fn start_in_place(
        &self,
        dir: PathBuf,
        replacing: Option<String>,
        stand_in: bool,
    ) -> Result<String, String> {
        let shared = self.shared();
        let found = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || {
                let fingerprint = frenz_model::fingerprint_dir(&dir)?;
                Pipeline::from_dir(&dir).map(|p| (p, fingerprint))
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?
        };
        let (pipeline, fingerprint) = found;
        let mut session = self.session.lock().await;
        let Some(session) = session.as_mut() else {
            return Err("Connect the band through openfrenz to run a pipeline on it.".into());
        };
        let Some(band) = session.band.as_ref() else {
            return Err("The band is not connected.".into());
        };
        let running = shared.models.lock().map(|m| m.clone()).unwrap_or_default();
        let (name, suffix) = match &replacing {
            Some(name) => {
                let suffix = running
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| suffix_of(name));
                (name.clone(), suffix)
            }
            None => {
                let taken: Vec<String> = running.into_iter().map(|(n, _)| n).collect();
                let name = unique_name(&pipeline.spec().name, &taken);
                let suffix = suffix_of(&name);
                (name, suffix)
            }
        };
        if replacing.is_some() {
            // The first copy to stand in keeps the original's folder, to revert to; reverting forgets it.
            if let (Ok(mut originals), Ok(dirs)) = (shared.originals.lock(), shared.dirs.lock()) {
                if !stand_in {
                    originals.remove(&name);
                } else if let (false, Some(current)) =
                    (originals.contains_key(&name), dirs.get(&name))
                {
                    originals.insert(name.clone(), current.clone());
                }
            }
            if let Some(task) = session.models.remove(&name) {
                task.abort();
            }
            let _ = session.lsl.send(ToLsl::CloseOne {
                suffix: suffix.clone(),
            });
            if let Ok(mut models) = shared.models.lock() {
                models.retain(|(n, _)| n != &name);
            }
            if let Ok(mut outputs) = shared.outputs.lock() {
                outputs.remove(&name);
            }
        }
        let copy = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let loaded = Loaded {
            source: if stand_in {
                format!(
                    "model {} {} (files {fingerprint}), in place of {name}",
                    dir.display(),
                    pipeline.spec().version
                )
            } else {
                format!(
                    "model {} {} (files {fingerprint})",
                    dir.display(),
                    pipeline.spec().version
                )
            },
            name: name.clone(),
            suffix,
            pipeline,
            dir,
        };
        let (stream, task) = start_model(loaded, band, session.reconnects, &session.lsl, &shared)?;
        session.models.insert(name.clone(), task);
        shared.update(|s| {
            if !s.streams.contains(&stream) {
                s.streams.push(stream);
            }
            if stand_in {
                s.stand_ins.insert(name.clone(), copy);
            } else {
                s.stand_ins.remove(&name);
            }
        });
        info!(device = "frenz", "model {name} started while connected");
        Ok(name)
    }

    /// The last `seconds` of every published stream and model output, for drawing.
    pub async fn recent(&self, seconds: f64) -> Vec<Trace> {
        let seconds = seconds.clamp(0.1, KEEP_SECONDS);
        let session = self.session.lock().await;
        let Some(session) = session.as_ref() else {
            return Vec::new();
        };
        let mut traces = Vec::new();
        for (stream, _) in STREAMS.iter().chain([("battery", "")].iter()) {
            let Some(chunk) =
                streams::index(stream).and_then(|index| session.streams.latest(index, seconds))
            else {
                continue;
            };
            let (t, rows) = rows_of(&chunk);
            let values = (0..chunk.channels.len())
                .map(|c| {
                    rows.iter()
                        .map(|row| row.get(c).copied().unwrap_or(f64::NAN))
                        .collect()
                })
                .collect();
            traces.push(Trace {
                name: (*stream).to_owned(),
                channels: chunk.channels.iter().map(|c| (*c).to_owned()).collect(),
                t,
                values,
            });
        }
        let shared = self.shared();
        let since = unix_now() - seconds;
        let running = shared.models.lock().map(|m| m.clone()).unwrap_or_default();
        if let Ok(preview) = shared.preview.lock() {
            if let Some((channels, kept)) = preview.as_ref() {
                let recent: Vec<&(f64, Vec<f64>)> =
                    kept.iter().filter(|(t, _)| *t >= since).collect();
                traces.push(Trace {
                    name: "preview".to_owned(),
                    channels: channels.clone(),
                    t: recent.iter().map(|(t, _)| *t).collect(),
                    values: (0..channels.len())
                        .map(|c| {
                            recent
                                .iter()
                                .map(|(_, v)| v.get(c).copied().unwrap_or(f64::NAN))
                                .collect()
                        })
                        .collect(),
                });
            }
        }
        if let (Ok(outputs), Ok(names)) = (shared.outputs.lock(), shared.output_names.lock()) {
            for (model, _) in &running {
                let (Some(kept), Some(channels)) = (outputs.get(model), names.get(model)) else {
                    continue;
                };
                let recent: Vec<&(f64, Vec<f64>)> =
                    kept.iter().filter(|(t, _)| *t >= since).collect();
                traces.push(Trace {
                    name: model.clone(),
                    channels: channels.clone(),
                    t: recent.iter().map(|(t, _)| *t).collect(),
                    values: (0..channels.len())
                        .map(|c| {
                            recent
                                .iter()
                                .map(|(_, v)| v.get(c).copied().unwrap_or(f64::NAN))
                                .collect()
                        })
                        .collect(),
                });
            }
        }
        traces
    }
}

/// A model to run: the name it is drawn under, its LSL suffix, the model, and where it came from.
struct Loaded {
    name: String,
    suffix: String,
    pipeline: Pipeline,
    source: String,
    /// The folder it was loaded from.
    dir: PathBuf,
}

/// `base` made into a model name (letters, digits and `-`), distinct from `taken`.
fn unique_name(base: &str, taken: &[String]) -> String {
    let base: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "model".to_owned()
    } else {
        base
    };
    // Names that differ only in case share an LSL suffix (it is the name in capitals), and two outlets under one
    // suffix end publishing for the whole connection. A band stream's name or the editor's trial trace would be
    // drawn twice in the window.
    let used = |name: &str| {
        taken.iter().any(|other| other.eq_ignore_ascii_case(name))
            || name.eq_ignore_ascii_case("preview")
            || STREAMS
                .iter()
                .any(|(stream, _)| stream.eq_ignore_ascii_case(name))
    };
    let mut name = base.clone();
    let mut n = 2;
    while used(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// The LSL suffix of an extra model: its name in capitals.
fn suffix_of(name: &str) -> String {
    format!("_{}_openfrenz", name.to_ascii_uppercase().replace('-', "_"))
}

/// A model source the configuration may name: a Hugging Face repository (`owner/name`,
/// `owner/name@revision`) or an existing folder.
fn plausible_source(source: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let (repo, revision) = source.split_once('@').unwrap_or((source, "main"));
    let repository = matches!(repo.split('/').collect::<Vec<_>>()[..], [owner, name] if plain(owner) && plain(name))
        && plain(revision);
    repository || std::path::Path::new(source).is_dir()
}

/// The built-in models, then the `extra` ones, each with a distinct name and suffix. Extra models that cannot
/// be loaded are left out, with why.
async fn load_models(extra: Vec<String>, token: Option<String>) -> (Vec<Loaded>, Vec<String>) {
    tokio::task::spawn_blocking(move || {
        let mut loaded: Vec<Loaded> = Vec::new();
        let mut errors = Vec::new();
        for (model, suffix) in MODELS {
            match frenz_model::resolve(model)
                .and_then(|dir| Pipeline::from_dir(&dir).map(|p| (p, dir)))
            {
                Ok((pipeline, dir)) => loaded.push(Loaded {
                    name: model.to_owned(),
                    suffix: suffix.to_owned(),
                    source: format!("model {model} {}", pipeline.spec().version),
                    pipeline,
                    dir,
                }),
                Err(e) => errors.push(format!("{model}: {e}")),
            }
        }
        for source in extra.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if !plausible_source(source) {
                errors.push(format!(
                    "{source}: not a Hugging Face repository (owner/name) or a folder"
                ));
                continue;
            }
            let pipeline = frenz_model::resolve_with(source, token.as_deref()).and_then(|dir| {
                let fingerprint = frenz_model::fingerprint_dir(&dir)?;
                Pipeline::from_dir(&dir).map(|p| (p, fingerprint, dir))
            });
            let (pipeline, fingerprint, dir) = match pipeline {
                Ok(found) => found,
                Err(e) => {
                    warn!(device = "frenz", "model {source} not loaded: {e}");
                    errors.push(format!("{source}: {e}"));
                    continue;
                }
            };
            // Named after the model, made unique; the suffix is the name in capitals.
            let taken: Vec<String> = loaded.iter().map(|l| l.name.clone()).collect();
            let name = unique_name(&pipeline.spec().name, &taken);
            let suffix = suffix_of(&name);
            loaded.push(Loaded {
                source: format!(
                    "model {source} {} (files {fingerprint})",
                    pipeline.spec().version
                ),
                name,
                suffix,
                pipeline,
                dir,
            });
        }
        (loaded, errors)
    })
    .await
    .unwrap_or_else(|e| (Vec::new(), vec![format!("loading models failed: {e}")]))
}

/// Puts each `(name, folder)` of `instead` in place of the loaded model `name`, keeping its name and stream. A
/// copy that cannot be loaded leaves the original running, with why. Returns `(name, original folder, copy's
/// folder name)` for each swapped.
async fn swap_in(
    models: &mut [Loaded],
    instead: &[(String, String)],
    errors: &mut Vec<String>,
) -> Vec<(String, PathBuf, String)> {
    let mut swapped = Vec::new();
    for (name, folder) in instead {
        let dir = PathBuf::from(folder);
        let copy = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| folder.clone());
        let Some(model) = models.iter_mut().find(|m| &m.name == name) else {
            errors.push(format!(
                "{copy}: {name} is not running, so it has nothing to replace"
            ));
            continue;
        };
        let loaded = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || {
                if !dir.is_dir() {
                    return Err(frenz_model::Error::Files("no such saved pipeline".into()));
                }
                let fingerprint = frenz_model::fingerprint_dir(&dir)?;
                Pipeline::from_dir(&dir).map(|p| (p, fingerprint))
            })
            .await
        };
        match loaded {
            Ok(Ok((pipeline, fingerprint))) => {
                model.source = format!(
                    "model {} {} (files {fingerprint}), in place of {name}",
                    dir.display(),
                    pipeline.spec().version
                );
                model.pipeline = pipeline;
                let original = std::mem::replace(&mut model.dir, dir);
                swapped.push((name.clone(), original, copy));
            }
            Ok(Err(e)) => errors.push(format!("{copy}: {e}; {name} runs as it comes")),
            Err(e) => errors.push(format!("{copy}: {e}; {name} runs as it comes")),
        }
    }
    swapped
}

/// Opens the band, the outlets and the tasks that feed them.
async fn open(
    device: &str,
    replay: Option<&std::path::Path>,
    models: Vec<Loaded>,
    shared: Arc<Shared>,
) -> Result<Session, String> {
    let band = match replay {
        Some(recording) => Band::replay(recording, ReplayOptions::default()).await,
        None => Band::connect(device, ConnectOptions::default()).await,
    }
    .map_err(|e| e.to_string())?;
    let reconnects = replay.is_none();

    let session_id = uuid::Uuid::new_v4().to_string();
    let (lsl, commands) = mpsc::channel();
    let thread = {
        let (device, session_id) = (device.to_owned(), session_id);
        std::thread::Builder::new()
            .name("frenz-openfrenz-lsl".into())
            .spawn(move || publish(device, session_id, commands))
            .map_err(|e| format!("could not start the LSL thread: {e}"))?
    };
    let open_outlet = |suffix: &str, channels: Vec<String>, rate_hz: f64, source: String| {
        open_outlet(&lsl, suffix, channels, rate_hz, source)
    };

    let mut published = Vec::new();
    let mut tasks = Vec::new();

    // Models first: on a replay each must subscribe before the recording starts to play.
    let mut models_running = BTreeMap::new();
    for loaded in models {
        let model = loaded.name.clone();
        let (name, task) = start_model(loaded, &band, reconnects, &lsl, &shared)?;
        published.push(name);
        models_running.insert(model, task);
    }

    // The band's own streams: one buffer that both the outlets and the window read.
    let buffers = Arc::new(Streams::new(KEEP_SECONDS));
    let pump = tokio::spawn(streams::pump(
        buffers.clone(),
        band.subscribe(),
        reconnects,
        End::Lost,
    ));
    tasks.push(tokio::spawn(async move {
        let _ = pump.await;
    }));
    for (stream, suffix) in STREAMS {
        let index =
            streams::index(stream).ok_or_else(|| format!("openfrenz has no {stream} stream"))?;
        let channels: Vec<String> = streams::channels(stream)
            .unwrap_or_default()
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        let rate = streams::rate_hz(stream).unwrap_or(0.0);
        let name = open_outlet(suffix, channels, rate, format!("stream {stream}"))?;
        published.push(name.clone());
        let (lsl, shared, buffers, first) = (
            lsl.clone(),
            shared.clone(),
            buffers.clone(),
            stream == "eeg",
        );
        tasks.push(tokio::spawn(async move {
            loop {
                match buffers.read(index).await {
                    Ok(chunk) => {
                        let (stamps, rows) = rows_of(&chunk);
                        if rows.is_empty() {
                            continue;
                        }
                        shared.count(&name, rows.len());
                        let _ = lsl.send(ToLsl::Push {
                            suffix: suffix.to_owned(),
                            stamps,
                            rows,
                        });
                    }
                    Err(end) => {
                        // Every stream ends together; one of them says so.
                        if first {
                            match end {
                                End::Finished => shared.update(|s| *s = NativeStatus::stopped()),
                                End::Lost => {
                                    shared.fail("The connection to the band was lost.".into())
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }));
    }

    shared.update(|s| {
        s.state = NativeState::Streaming;
        s.streams = published;
        s.message = None;
    });
    info!(device = "frenz", "openfrenz connected to {device}");
    Ok(Session {
        band: Some(band),
        reconnects,
        streams: buffers,
        tasks,
        models: models_running,
        preview: None,
        lsl,
        thread: Some(thread),
    })
}

/// Asks the publishing thread for an outlet; returns the stream's name.
fn open_outlet(
    lsl: &mpsc::Sender<ToLsl>,
    suffix: &str,
    channels: Vec<String>,
    rate_hz: f64,
    source: String,
) -> Result<String, String> {
    let (reply, answer) = mpsc::channel();
    lsl.send(ToLsl::Open {
        suffix: suffix.to_owned(),
        channels,
        rate_hz,
        source,
        reply,
    })
    .map_err(|_| "the LSL thread stopped".to_owned())?;
    answer
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "the LSL thread did not answer".to_owned())?
}

/// Opens a model's outlet and starts it on `band`: each output is kept for drawing and published. Returns the
/// stream's name and the task.
fn start_model(
    loaded: Loaded,
    band: &Band,
    reconnects: bool,
    lsl: &mpsc::Sender<ToLsl>,
    shared: &Arc<Shared>,
) -> Result<(String, JoinHandle<()>), String> {
    let Loaded {
        name: model,
        suffix,
        pipeline,
        source,
        dir,
    } = loaded;
    let names = pipeline.spec().outputs.clone();
    let name = open_outlet(lsl, &suffix, names.clone(), 0.0, source)?;
    if let Ok(mut all) = shared.output_names.lock() {
        all.insert(model.clone(), names);
    }
    if let Ok(mut running) = shared.models.lock() {
        running.push((model.clone(), suffix.clone()));
    }
    if let Ok(mut dirs) = shared.dirs.lock() {
        dirs.insert(model.clone(), dir);
    }
    let mut live = Live::new(pipeline, band, reconnects).map_err(|e| format!("{model}: {e}"))?;
    let (lsl, shared, stream) = (lsl.clone(), shared.clone(), name.clone());
    let task = tokio::spawn(async move {
        loop {
            match live.next().await {
                Ok(Some(output)) => {
                    shared.count(&stream, 1);
                    if let Ok(mut all) = shared.outputs.lock() {
                        let kept = all.entry(model.clone()).or_default();
                        kept.push_back((output.t, output.values.clone()));
                        while kept.len() > KEEP_OUTPUTS {
                            kept.pop_front();
                        }
                    }
                    let _ = lsl.send(ToLsl::Push {
                        suffix: suffix.clone(),
                        stamps: vec![output.t],
                        rows: vec![output.values],
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(device = "frenz", "model {model} stopped: {e}");
                    break;
                }
            }
        }
    });
    Ok((name, task))
}
