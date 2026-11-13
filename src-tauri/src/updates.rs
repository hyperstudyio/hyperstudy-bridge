//! Update checks.
//!
//! Bridge checks for an update on every launch, from Rust startup rather than from the page: the page-side check
//! waited three seconds after mount, failed silently, and left no trace, so an installed 0.8.26 sat out two releases
//! with nothing to show why. Every outcome is now logged, kept as the current status for the UI, and pushed to it as
//! an `update-status` event.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Only the released app may replace itself; a development build would be overwritten by the production one.
pub const RELEASE_IDENTIFIER: &str = "com.hyperstudy.bridge";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateStatus {
    NotChecked,
    Checking,
    UpToDate {
        version: String,
    },
    Available {
        version: String,
        current_version: String,
        notes: Option<String>,
    },
    Failed {
        error: String,
    },
    Installing,
}

#[derive(Default)]
pub struct UpdateState(RwLock<Option<UpdateStatus>>);

impl UpdateState {
    pub async fn get(&self) -> UpdateStatus {
        self.0
            .read()
            .await
            .clone()
            .unwrap_or(UpdateStatus::NotChecked)
    }
}

/// What a check found, as the status the UI shows.
pub fn status_from_check(
    current_version: &str,
    found: Result<Option<(String, Option<String>)>, String>,
) -> UpdateStatus {
    match found {
        Ok(Some((version, notes))) => UpdateStatus::Available {
            version,
            current_version: current_version.to_string(),
            notes,
        },
        Ok(None) => UpdateStatus::UpToDate {
            version: current_version.to_string(),
        },
        Err(error) => UpdateStatus::Failed { error },
    }
}

/// Why installing now would be wrong, if it would be. Installing restarts Bridge, which would cut a recording short.
pub fn install_blocker(identifier: &str, recorder_state: &str) -> Option<String> {
    if identifier != RELEASE_IDENTIFIER {
        return Some(
            "This development build doesn't install updates. Install the release from GitHub."
                .into(),
        );
    }
    if !restart_now(recorder_state) {
        return Some("A recording is running. Install the update after it ends.".into());
    }
    None
}

/// Whether Bridge may restart now: never while the recorder is starting, recording or finalizing.
pub fn restart_now(recorder_state: &str) -> bool {
    !matches!(recorder_state, "starting" | "recording" | "stopping")
}

async fn publish(app: &AppHandle, status: UpdateStatus) {
    *app.state::<UpdateState>().0.write().await = Some(status.clone());
    let _ = app.emit("update-status", &status);
}

/// Checks for an update, logs the outcome and publishes it. Runs on every launch and from Help > Check for Updates.
pub async fn check(app: &AppHandle) -> UpdateStatus {
    publish(app, UpdateStatus::Checking).await;
    let current = app.package_info().version.to_string();
    let found = match app.updater() {
        Ok(updater) => updater
            .check()
            .await
            .map(|update| update.map(|u| (u.version, u.body)))
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let status = status_from_check(&current, found);
    match &status {
        UpdateStatus::Available { version, .. } => {
            info!("Update check: {version} is available (running {current})")
        }
        UpdateStatus::UpToDate { .. } => info!("Update check: {current} is the latest version"),
        UpdateStatus::Failed { error } => warn!("Update check failed (running {current}): {error}"),
        _ => {}
    }
    publish(app, status.clone()).await;
    status
}

/// Downloads, installs and restarts. Progress goes out as `update-progress` events (downloaded and total bytes).
/// `recorder_state` is read before downloading and again before installing, because a study can start recording
/// during the download, and installing ends Bridge (on Windows the installer quits it; elsewhere it restarts).
pub async fn install(app: &AppHandle, recorder_state: impl Fn() -> String) -> Result<(), String> {
    if let Some(reason) = install_blocker(&app.config().identifier, &recorder_state()) {
        return Err(reason);
    }
    let found = match app.updater() {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    // A failed or empty re-check is the current answer; publish it so the page stops offering this version.
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => {
            let version = app.package_info().version.to_string();
            publish(app, UpdateStatus::UpToDate { version }).await;
            return Err("Bridge is already up to date".into());
        }
        Err(error) => {
            publish(
                app,
                UpdateStatus::Failed {
                    error: error.clone(),
                },
            )
            .await;
            return Err(error);
        }
    };
    info!("Downloading update {}", update.version);
    publish(app, UpdateStatus::Installing).await;
    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    // download() verifies the signature before returning the bytes.
    let bytes = match update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app.emit(
                    "update-progress",
                    serde_json::json!({ "downloaded": downloaded, "total": total }),
                );
            },
            || {},
        )
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => return fail(app, format!("Update download failed: {e}")).await,
    };
    if !restart_now(&recorder_state()) {
        info!(
            "Update {} not installed: a recording started during the download",
            update.version
        );
        let current = app.package_info().version.to_string();
        let offer = status_from_check(
            &current,
            Ok(Some((update.version.clone(), update.body.clone()))),
        );
        publish(app, offer).await;
        return Err(
            "A recording started while the update downloaded. Install it after the recording ends."
                .into(),
        );
    }
    if let Err(e) = update.install(&bytes) {
        return fail(app, format!("Update install failed: {e}")).await;
    }
    info!("Update {} installed; restarting", update.version);
    app.restart();
}

async fn fail(app: &AppHandle, error: String) -> Result<(), String> {
    warn!("{error}");
    publish(
        app,
        UpdateStatus::Failed {
            error: error.clone(),
        },
    )
    .await;
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_maps_to_the_status_the_ui_shows() {
        assert_eq!(
            status_from_check("0.8.27", Ok(Some(("0.8.28".into(), Some("notes".into()))))),
            UpdateStatus::Available {
                version: "0.8.28".into(),
                current_version: "0.8.27".into(),
                notes: Some("notes".into())
            }
        );
        assert_eq!(
            status_from_check("0.8.28", Ok(None)),
            UpdateStatus::UpToDate {
                version: "0.8.28".into()
            }
        );
        assert_eq!(
            status_from_check("0.8.28", Err("offline".into())),
            UpdateStatus::Failed {
                error: "offline".into()
            }
        );
    }

    #[test]
    fn installing_never_cuts_a_recording_or_replaces_a_dev_build() {
        for state in ["starting", "recording", "stopping"] {
            assert!(install_blocker(RELEASE_IDENTIFIER, state)
                .unwrap()
                .contains("recording is running"));
        }
        assert!(
            install_blocker("com.hyperstudy.bridge.recording-dev", "idle")
                .unwrap()
                .contains("development")
        );
        assert_eq!(install_blocker(RELEASE_IDENTIFIER, "idle"), None);
        assert_eq!(install_blocker(RELEASE_IDENTIFIER, "completed"), None);
    }

    /// A recording can start while the update downloads (HyperStudy starts studies over the WebSocket). Installing
    /// ends Bridge, so the recorder is checked again between download and install.
    #[test]
    fn a_recording_that_starts_during_the_download_is_never_cut_short() {
        for state in ["starting", "recording", "stopping"] {
            assert!(!restart_now(state));
        }
        for state in ["idle", "completed", "failed", "cancelled"] {
            assert!(restart_now(state));
        }
    }

    #[test]
    fn status_serializes_with_a_state_tag_for_the_page() {
        let json = serde_json::to_value(UpdateStatus::Failed {
            error: "offline".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "state": "failed", "error": "offline" })
        );
    }
}
