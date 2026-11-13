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
    if matches!(recorder_state, "starting" | "recording" | "stopping") {
        return Some("A recording is running. Install the update after it ends.".into());
    }
    None
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

/// Downloads and installs the available update, then restarts Bridge. Progress goes out as `update-progress`
/// events with the downloaded and total byte counts.
pub async fn install(app: &AppHandle, recorder_state: &str) -> Result<(), String> {
    if let Some(reason) = install_blocker(&app.config().identifier, recorder_state) {
        return Err(reason);
    }
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Bridge is already up to date")?;
    info!("Installing update {}", update.version);
    publish(app, UpdateStatus::Installing).await;
    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app.emit(
                    "update-progress",
                    serde_json::json!({ "downloaded": downloaded, "total": total }),
                );
            },
            || {},
        )
        .await;
    if let Err(e) = result {
        warn!("Update install failed: {e}");
        let status = UpdateStatus::Failed {
            error: e.to_string(),
        };
        publish(app, status).await;
        return Err(e.to_string());
    }
    info!("Update {} installed; restarting", update.version);
    app.restart();
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
