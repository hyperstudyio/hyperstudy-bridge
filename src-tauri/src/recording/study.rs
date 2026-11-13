//! Managed study sessions. The browser owns an identity, never a filesystem path.
use super::*;
use anyhow::Context;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudySession {
    pub recording_id: String,
    pub created_at: String,
    pub context: RecordingContext,
    #[serde(flatten)]
    pub recording: RecordingStatus,
    pub upload_status: String,
    pub upload_error: Option<String>,
}
#[derive(Default)]
pub(super) struct StudyRegistry {
    directory: Option<PathBuf>,
    sessions: BTreeMap<String, StudySession>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyStartRequest {
    pub stream_uids: Vec<String>,
    pub context: RecordingContext,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyScope {
    pub recording_id: String,
    pub context: RecordingContext,
    pub end_marker: Option<Value>,
}
pub(super) fn validate_context(c: &RecordingContext) -> Result<()> {
    for field in [
        &c.experiment_id,
        &c.room_id,
        &c.participant_id,
        &c.session_id,
    ] {
        ensure!(
            field
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty() && s.len() <= 512),
            "Complete study identity is required"
        );
    }
    Ok(())
}
pub(super) fn same_context(a: &RecordingContext, b: &RecordingContext) -> bool {
    a.experiment_id == b.experiment_id
        && a.room_id == b.room_id
        && a.participant_id == b.participant_id
        && a.session_id == b.session_id
}
fn active(state: &str) -> bool {
    matches!(state, "starting" | "recording" | "stopping")
}
impl StudyRegistry {
    fn persist(&self, session: &StudySession) -> Result<()> {
        let directory = self
            .directory
            .as_ref()
            .context("Study recording directory not initialized")?;
        let path = directory.join(format!("{}.session.json", session.recording_id));
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec(session)?)?;
        super::store::sync_file(&temp)?;
        std::fs::rename(temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
    fn refresh(&mut self, status: &RecordingStatus) -> Result<()> {
        if let Some(session) = self
            .sessions
            .values_mut()
            .find(|s| s.recording.path == status.path && status.path.is_some())
        {
            session.recording = status.clone();
            let session = session.clone();
            self.persist(&session)?;
        }
        Ok(())
    }
}
impl RecordingManager {
    /// Called once during app setup; capabilities are deliberately absent from disk.
    pub fn configure_study_directory(&self, directory: PathBuf) -> Result<()> {
        std::fs::create_dir_all(&directory)?;
        let mut registry = self.study.lock().unwrap();
        ensure!(
            registry.directory.is_none(),
            "Study recording directory already initialized"
        );
        registry.directory = Some(directory.clone());
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if !path
                .file_name()
                .is_some_and(|s| s.to_string_lossy().ends_with(".session.json"))
            {
                continue;
            }
            let mut session: StudySession = serde_json::from_slice(&std::fs::read(&path)?)?;
            uuid::Uuid::parse_str(&session.recording_id)?;
            ensure!(
                session.recording.path
                    == Some(directory.join(format!("{}.sqlite", session.recording_id))),
                "Invalid managed recording path"
            );
            if active(&session.recording.state) {
                // A process restart cannot resume native acquisition. Retain WAL and
                // original file; local recovery is explicit and creates a new file.
                session.recording.state = "interrupted".into();
                session.recording.error = Some(
                    "Bridge restarted during acquisition; original file retained for recovery"
                        .into(),
                );
            }
            if matches!(session.upload_status.as_str(), "uploading" | "verifying") {
                session.upload_status = "pending".into();
                session.upload_error =
                    Some("Bridge restarted; fresh upload authorization is required".into());
            }
            registry.persist(&session)?;
            registry
                .sessions
                .insert(session.recording_id.clone(), session);
        }
        Ok(())
    }
    pub fn start_study(&self, request: StudyStartRequest) -> Result<StudySession> {
        validate_context(&request.context)?;
        let mut registry = self.study.lock().unwrap();
        registry.refresh(&self.status())?;
        if let Some(session) = registry
            .sessions
            .values()
            .find(|s| same_context(&s.context, &request.context) && active(&s.recording.state))
        {
            return Ok(session.clone());
        }
        ensure!(
            !active(&self.status().state),
            "Another recording is active; reconnect with its study identity or stop locally"
        );
        let directory = registry
            .directory
            .clone()
            .context("Study recording directory not initialized")?;
        let recording_id = uuid::Uuid::new_v4().to_string();
        let mut session = StudySession {
            recording_id: recording_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context: request.context.clone(),
            recording: RecordingStatus {
                state: "starting".into(),
                path: Some(directory.join(format!("{recording_id}.sqlite"))),
                ..Default::default()
            },
            upload_status: "local".into(),
            upload_error: None,
        };
        // Persist identity before native setup, so a crash never hides the file
        // from the managed-session inventory.
        registry.persist(&session)?;
        registry
            .sessions
            .insert(recording_id.clone(), session.clone());
        match self.start_internal(
            RecordingRequest {
                stream_uids: request.stream_uids,
                directory,
                context: request.context,
            },
            true,
            Some(recording_id.clone()),
        ) {
            Ok(status) => session.recording = status,
            Err(error) => {
                session.recording.state = "failed".into();
                session.recording.error = Some(error.to_string());
                registry.persist(&session)?;
                registry.sessions.insert(recording_id, session);
                return Err(error);
            }
        }
        registry.persist(&session)?;
        registry.sessions.insert(recording_id, session.clone());
        Ok(session)
    }
    pub fn list_studies(&self) -> Result<Vec<StudySession>> {
        let mut registry = self.study.lock().unwrap();
        registry.refresh(&self.status())?;
        let mut sessions: Vec<_> = registry.sessions.values().cloned().collect();
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(sessions)
    }
    /// The file of a recording Bridge made: a study recording by id, or the manual recorder's file. Callers reveal
    /// it in Finder, so it never resolves a path the frontend supplies.
    pub fn recorded_file(&self, recording_id: Option<&str>) -> Result<PathBuf> {
        let path = match recording_id {
            Some(id) => self
                .list_studies()?
                .into_iter()
                .find(|s| s.recording_id == id)
                .and_then(|s| s.recording.path)
                .ok_or_else(|| anyhow::anyhow!("That recording is no longer listed"))?,
            None => self
                .status()
                .path
                .ok_or_else(|| anyhow::anyhow!("Nothing has been recorded yet"))?,
        };
        ensure!(
            path.exists(),
            "The recording file is no longer at {}",
            path.display()
        );
        Ok(path)
    }
    pub fn get_study(&self, context: &RecordingContext) -> Result<Option<StudySession>> {
        validate_context(context)?;
        let sessions = self.list_studies()?;
        // Active match first; otherwise return the most recently started session.
        Ok(sessions
            .iter()
            .find(|s| same_context(&s.context, context) && active(&s.recording.state))
            .or_else(|| {
                sessions
                    .iter()
                    .rev()
                    .find(|s| same_context(&s.context, context))
            })
            .cloned())
    }
    pub fn stop_study(&self, request: StudyScope) -> Result<StudySession> {
        validate_context(&request.context)?;
        let mut registry = self.study.lock().unwrap();
        registry.refresh(&self.status())?;
        let existing = registry
            .sessions
            .get(&request.recording_id)
            .context("Unknown study recording")?
            .clone();
        ensure!(
            same_context(&existing.context, &request.context),
            "Recording context does not match"
        );
        if !active(&existing.recording.state) {
            return Ok(existing);
        }
        ensure!(
            existing.recording.path == self.status().path,
            "Recording is not the active session"
        );
        if let Some(marker) = &request.end_marker {
            ensure!(
                serde_json::to_vec(marker)?.len() <= 64 * 1024,
                "Marker exceeds 64 KiB"
            );
        }
        let mut signal = StopSignal::new(RecordingStopRequest {
            boundaries: vec![],
            timeout_ms: 8_000,
        });
        signal.map_local_boundary = request.end_marker.is_some();
        signal.end_marker = request.end_marker;
        if !signal.map_local_boundary {
            signal.request.timeout_ms = 0;
        }
        let status = self.stop_signal_scoped(signal, existing.recording.path.as_deref())?;
        registry.refresh(&status)?;
        Ok(registry
            .sessions
            .get(&request.recording_id)
            .unwrap()
            .clone())
    }
    pub fn study_marker(
        &self,
        id: &str,
        context: &RecordingContext,
        marker: Value,
    ) -> Result<Value> {
        validate_context(context)?;
        ensure!(
            serde_json::to_vec(&marker)?.len() <= 64 * 1024,
            "Marker exceeds 64 KiB"
        );
        let registry = self.study.lock().unwrap();
        let session = registry
            .sessions
            .get(id)
            .context("Unknown study recording")?;
        ensure!(
            same_context(&session.context, context),
            "Recording context does not match"
        );
        let active = self.active.lock().unwrap();
        ensure!(
            session.recording.path == self.status().path && self.status().state == "recording",
            "Recording is not active"
        );
        let (tx, rx) = mpsc::channel();
        active
            .as_ref()
            .context("Recorder worker is unavailable")?
            .stop
            .send(WorkerCommand::Marker { marker, reply: tx })?;
        let timestamp = rx
            .recv()
            .context("Recorder did not acknowledge marker")?
            .map_err(anyhow::Error::msg)?;
        Ok(json!({ "recording_id": id, "marker_lsl_timestamp": timestamp }))
    }
    /// Legacy WebSocket callers cannot bypass study ownership. The native
    /// RecordingPanel deliberately keeps the explicit local stop control.
    pub fn stop_unmanaged(&self, request: RecordingStopRequest) -> Result<RecordingStatus> {
        let registry = self.study.lock().unwrap();
        let status = self.status();
        ensure!(
            !active(&status.state)
                || !registry
                    .sessions
                    .values()
                    .any(|s| s.recording.path == status.path),
            "Study recordings require recording_id and context; use scoped stop or stop locally"
        );
        self.stop_with(request)
    }
    pub fn study_path(&self, id: &str) -> Result<PathBuf> {
        let mut registry = self.study.lock().unwrap();
        registry.refresh(&self.status())?;
        let session = registry
            .sessions
            .get(id)
            .context("Unknown study recording")?;
        ensure!(
            session.recording.state == "completed",
            "Only finalized recordings can be uploaded"
        );
        session
            .recording
            .path
            .clone()
            .context("Recording file unavailable")
    }
    pub fn set_upload_status(
        &self,
        id: &str,
        state: &str,
        error: Option<String>,
    ) -> Result<StudySession> {
        let mut registry = self.study.lock().unwrap();
        let session = registry
            .sessions
            .get_mut(id)
            .context("Unknown study recording")?;
        session.upload_status = state.into();
        session.upload_error = error;
        let session = session.clone();
        registry.persist(&session)?;
        Ok(session)
    }
    pub fn begin_upload(&self, id: &str) -> Result<StudySession> {
        let mut registry = self.study.lock().unwrap();
        let session = registry
            .sessions
            .get_mut(id)
            .context("Unknown study recording")?;
        ensure!(
            !matches!(session.upload_status.as_str(), "uploading" | "verifying"),
            "Upload already active"
        );
        ensure!(
            session.recording.state == "completed",
            "Only finalized recordings can be uploaded"
        );
        session.upload_status = "uploading".into();
        session.upload_error = None;
        let session = session.clone();
        registry.persist(&session)?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> RecordingContext {
        RecordingContext {
            experiment_id: Some("e".into()),
            room_id: Some("r".into()),
            participant_id: Some("p".into()),
            session_id: Some("s".into()),
            label: None,
        }
    }
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!("study-session-test-{}", uuid::Uuid::new_v4()))
    }
    #[test]
    fn restart_preserves_local_file_and_requires_fresh_upload_authorization() {
        let directory = directory();
        let manager = RecordingManager::default();
        manager
            .configure_study_directory(directory.clone())
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = directory.join(format!("{id}.sqlite"));
        std::fs::write(&path, b"original bytes").unwrap();
        let session = StudySession {
            recording_id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context: context(),
            recording: RecordingStatus {
                state: "completed".into(),
                path: Some(path.clone()),
                ..Default::default()
            },
            upload_status: "uploading".into(),
            upload_error: None,
        };
        manager.study.lock().unwrap().persist(&session).unwrap();
        drop(manager);
        let restarted = RecordingManager::default();
        restarted.configure_study_directory(directory).unwrap();
        let sessions = restarted.list_studies().unwrap();
        assert_eq!(sessions[0].upload_status, "pending");
        assert_eq!(std::fs::read(path).unwrap(), b"original bytes");
        assert!(sessions[0].upload_error.as_ref().unwrap().contains("fresh"));
    }
    #[test]
    fn recorded_file_resolves_only_recordings_bridge_made() {
        let directory = directory();
        let manager = RecordingManager::default();
        manager
            .configure_study_directory(directory.clone())
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = directory.join(format!("{id}.sqlite"));
        std::fs::write(&path, b"x").unwrap();
        let session = StudySession {
            recording_id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context: context(),
            recording: RecordingStatus {
                state: "completed".into(),
                path: Some(path.clone()),
                ..Default::default()
            },
            upload_status: "complete".into(),
            upload_error: None,
        };
        manager
            .study
            .lock()
            .unwrap()
            .sessions
            .insert(id.clone(), session);
        assert_eq!(manager.recorded_file(Some(&id)).unwrap(), path);
        assert!(manager.recorded_file(Some("../../etc/passwd")).is_err());
        assert!(
            manager.recorded_file(None).is_err(),
            "no manual recording yet"
        );
        std::fs::remove_file(&path).unwrap();
        assert!(manager
            .recorded_file(Some(&id))
            .unwrap_err()
            .to_string()
            .contains("no longer at"));
    }
    #[test]
    fn wrong_identity_cannot_stop_or_mark_and_duplicate_stop_is_idempotent() {
        let manager = RecordingManager::default();
        manager.configure_study_directory(directory()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let session = StudySession {
            recording_id: id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context: context(),
            recording: RecordingStatus {
                state: "completed".into(),
                ..Default::default()
            },
            upload_status: "local".into(),
            upload_error: None,
        };
        manager
            .study
            .lock()
            .unwrap()
            .sessions
            .insert(id.clone(), session);
        let mut wrong = context();
        wrong.participant_id = Some("other".into());
        assert!(manager
            .stop_study(StudyScope {
                recording_id: id.clone(),
                context: wrong.clone(),
                end_marker: None
            })
            .is_err());
        assert!(manager
            .study_marker(&id, &wrong, json!({"event":"end"}))
            .is_err());
        assert_eq!(
            manager
                .stop_study(StudyScope {
                    recording_id: id.clone(),
                    context: context(),
                    end_marker: Some(json!({"event":"end"}))
                })
                .unwrap()
                .recording
                .state,
            "completed"
        );
        assert_eq!(
            manager
                .stop_study(StudyScope {
                    recording_id: id,
                    context: context(),
                    end_marker: None
                })
                .unwrap()
                .recording
                .state,
            "completed"
        );
    }
    #[tokio::test]
    async fn detached_completion_job_preserves_file_and_never_persists_capability() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for http_status in [200, 500] {
            let manager = Arc::new(RecordingManager::default());
            let directory = directory();
            manager
                .configure_study_directory(directory.clone())
                .unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let path = directory.join(format!("{id}.sqlite"));
            let spec = StreamDescriptor {
                uid: "test".into(),
                name: "test".into(),
                source_id: "test".into(),
                channels: 1,
                format: "string".into(),
                nominal_rate: 0.,
                xml: "<info/>".into(),
            };
            let store = Store::create(&path, &context(), &[spec]).unwrap();
            store.finish(&path, "completed").unwrap();
            let metadata = super::super::upload::manifest(&path, &id).unwrap();
            let session = StudySession {
                recording_id: id.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                context: context(),
                recording: RecordingStatus {
                    state: "completed".into(),
                    path: Some(path.clone()),
                    ..Default::default()
                },
                upload_status: "pending".into(),
                upload_error: None,
            };
            manager
                .study
                .lock()
                .unwrap()
                .sessions
                .insert(id.clone(), session);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let response = json!({"data":[{"recordingId":id,"status":"complete","sha256":metadata["sha256"],"fileSize":metadata["fileSize"]}]}).to_string();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut received = vec![0; 8192];
                let n = socket.read(&mut received).await.unwrap();
                let request = String::from_utf8_lossy(&received[..n]);
                assert!(request.contains("x-recording-upload-token: memory-only-secret"));
                let header = format!("HTTP/1.1 {http_status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len());
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            let grant = serde_json::from_value(json!({"recordingId":id,"upload":null,"completion":{"url":format!("/api/v3/data/device-recordings/room/r/uploads/{id}/complete"),"token":"memory-only-secret","expiresAt":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}})).unwrap();
            super::super::upload::enqueue(manager.clone(), id.clone(), base, grant)
                .await
                .unwrap();
            server.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let status = manager.list_studies().unwrap().remove(0).upload_status;
                    if status == "complete" || status == "pending" {
                        assert_eq!(
                            status,
                            if http_status == 200 {
                                "complete"
                            } else {
                                "pending"
                            }
                        );
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let persisted =
                std::fs::read_to_string(directory.join(format!("{id}.session.json"))).unwrap();
            assert!(!persisted.contains("memory-only-secret"));
            assert_eq!(
                super::super::upload::manifest(&path, &id).unwrap()["sha256"],
                metadata["sha256"]
            );
        }
    }
}
