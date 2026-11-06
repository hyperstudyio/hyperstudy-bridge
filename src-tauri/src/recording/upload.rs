//! Narrow, memory-only upload capabilities; finalized files are always retained.
use super::{RecordingContext, RecordingManager, StreamDescriptor};
use anyhow::{ensure, Context, Result};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path, sync::Arc, time::Duration};

pub fn manifest(path: &Path, id: &str) -> Result<Value> {
    let size = std::fs::metadata(path)?.len();
    ensure!(
        size > 0 && size <= 1024 * 1024 * 1024,
        "Recording must be at most 1 GiB"
    );
    // No automatic recovery/checkpoint: an upload must be a standalone artifact.
    ensure!(
        !path.with_extension("sqlite-wal").exists()
            || std::fs::metadata(path.with_extension("sqlite-wal"))?.len() == 0,
        "Recording still has a WAL; finalize or recover locally first"
    );
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let app: i32 = db.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        app == super::store::APPLICATION_ID && version == 1,
        "Unsupported recording format"
    );
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "Recording integrity check failed");
    let (state, context, count, started, ended, report): (String,String,u64,String,Option<String>,Option<String>) = db.query_row(
        "SELECT state,context_json,committed_samples,started_utc,ended_utc,end_report_json FROM recording WHERE id=1", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    ensure!(
        state == "completed" && ended.is_some(),
        "Recording is not finalized"
    );
    let context: RecordingContext = serde_json::from_str(&context)?;
    super::study::validate_context(&context)?;
    let block_count: u64 = db.query_row(
        "SELECT COALESCE(SUM(sample_count),0) FROM blocks",
        [],
        |r| r.get(0),
    )?;
    ensure!(block_count == count, "Recording sample count mismatch");
    let mut statement = db.prepare("SELECT metadata_json FROM streams ORDER BY id")?;
    let mut streams = Vec::new();
    for row in statement.query_map([], |r| r.get::<_, String>(0))? {
        let descriptor: StreamDescriptor = serde_json::from_str(&row?)?;
        streams.push(json!({ "uid": descriptor.uid, "name": descriptor.name, "source_id": descriptor.source_id,
            "channels": descriptor.channels, "format": descriptor.format, "nominal_rate": descriptor.nominal_rate }));
    }
    let report: Option<Value> = report.map(|s| serde_json::from_str(&s)).transpose()?;
    drop(statement);
    drop(db);
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(
        json!({"recordingId":id, "experimentId":context.experiment_id, "participantId":context.participant_id,
        "fileSize":size, "sha256":format!("{:x}",hasher.finalize()), "startedAt":started,"endedAt":ended,
        "streams":streams,"sampleCount":count,"endReport":report}),
    )
}
pub(super) fn validate_api_base(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("Invalid API base URL")?;
    let host = url.host_str().unwrap_or("");
    let trusted = matches!(
        host,
        "hyperstudy.io" | "dev.hyperstudy.io" | "api.hyperstudy.io" | "dev-api.hyperstudy.io"
    );
    let local = cfg!(debug_assertions) && matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    ensure!(
        (url.scheme() == "https" && trusted && url.port_or_known_default() == Some(443))
            || (local && matches!(url.scheme(), "http" | "https")),
        "Untrusted HyperStudy API origin"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "API base must be an origin without credentials"
    );
    Ok(url)
}
pub(super) fn validate_upload_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("Invalid cloud upload URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("storage.googleapis.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Untrusted cloud upload target"
    );
    Ok(url)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadGrant {
    pub recording_id: String,
    pub upload: Option<PutGrant>,
    pub completion: CompletionGrant,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutGrant {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub expires_at: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionGrant {
    pub url: String,
    pub token: String,
    pub expires_at: String,
}
fn check_expiry(value: &str) -> Result<()> {
    ensure!(
        chrono::DateTime::parse_from_rfc3339(value).context("Invalid capability expiry")?
            > chrono::Utc::now(),
        "Upload authorization expired; request a fresh grant"
    );
    Ok(())
}
fn validate_completed_metadata(response: &Value, metadata: &Value, id: &str) -> Result<()> {
    let record = response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .context("Cloud verification returned no artifact")?;
    ensure!(
        record["recordingId"] == id
            && record["status"] == "complete"
            && record["sha256"] == metadata["sha256"]
            && record["fileSize"] == metadata["fileSize"],
        "Cloud verification response does not match this artifact"
    );
    Ok(())
}
fn validate_grant(base: &str, grant: &UploadGrant, id: &str, room: &str, size: u64) -> Result<Url> {
    ensure!(grant.recording_id == id, "Upload grant recording mismatch");
    let origin = validate_api_base(base)?;
    let mut expected = origin.clone();
    expected
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid API origin"))?
        .extend([
            "api",
            "v3",
            "data",
            "device-recordings",
            "room",
            room,
            "uploads",
            id,
            "complete",
        ]);
    ensure!(
        grant.completion.url == expected.path(),
        "Completion path does not match this recording"
    );
    ensure!(
        !grant.completion.token.is_empty() && grant.completion.token.len() <= 8192,
        "Invalid completion capability"
    );
    check_expiry(&grant.completion.expires_at)?;
    if let Some(upload) = &grant.upload {
        validate_upload_url(&upload.url)?;
        check_expiry(&upload.expires_at)?;
        ensure!(upload.method == "PUT", "Invalid cloud upload method");
        let headers: BTreeMap<_, _> = upload
            .headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.as_str()))
            .collect();
        ensure!(
            headers.len() == 3
                && upload.headers.len() == 3
                && headers.get("content-type") == Some(&"application/vnd.sqlite3")
                && headers.get("x-goog-if-generation-match") == Some(&"0")
                && headers.get("x-goog-content-length-range")
                    == Some(&format!("{size},{size}").as_str()),
            "Upload headers do not bind this immutable artifact"
        );
    }
    Ok(expected)
}
/// All disk validation/hashing happens in a blocking worker; HTTP streams bounded
/// chunks and follows no redirects. No retry loop and no stored bearer material.
pub async fn enqueue(
    manager: Arc<RecordingManager>,
    id: String,
    base: String,
    grant: UploadGrant,
) -> Result<Value> {
    let m = manager.clone();
    let record_id = id.clone();
    let (path, metadata, room) = tokio::task::spawn_blocking(move || -> Result<_> {
        let path = m.study_path(&record_id)?;
        let metadata = manifest(&path, &record_id)?;
        let session = m
            .list_studies()?
            .into_iter()
            .find(|s| s.recording_id == record_id)
            .context("Unknown study recording")?;
        Ok((
            path,
            metadata,
            session.context.room_id.context("Room missing")?,
        ))
    })
    .await??;
    let size = metadata["fileSize"].as_u64().context("Invalid file size")?;
    let completion = validate_grant(&base, &grant, &id, &room, size)?;
    let m = manager.clone();
    let record_id = id.clone();
    let status = tokio::task::spawn_blocking(move || m.begin_upload(&record_id)).await??;
    tokio::spawn(async move {
        let result: Result<()> = async {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(3600))
                .build()?;
            if let Some(upload) = grant.upload {
                let file = tokio::fs::File::open(&path).await?;
                let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::with_capacity(
                    file,
                    128 * 1024,
                ));
                let mut request = client
                    .put(upload.url)
                    .header(reqwest::header::CONTENT_LENGTH, size);
                for (key, value) in upload.headers {
                    request = request.header(key, value);
                }
                let response = request
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| e.without_url())?;
                ensure!(
                    response.status().is_success(),
                    "Cloud upload failed with HTTP {}; local file retained",
                    response.status().as_u16()
                );
            }
            let m = manager.clone();
            let record_id = id.clone();
            tokio::task::spawn_blocking(move || m.set_upload_status(&record_id, "verifying", None))
                .await??;
            let mut response = client
                .post(completion)
                .header("X-Recording-Upload-Token", grant.completion.token)
                .json(&json!({}))
                .send()
                .await
                .map_err(|e| e.without_url())?;
            ensure!(
                response.status().is_success(),
                "Cloud verification failed with HTTP {}; local file retained",
                response.status().as_u16()
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
                ensure!(
                    bytes.len() + chunk.len() <= 64 * 1024,
                    "Cloud verification response exceeds 64 KiB"
                );
                bytes.extend_from_slice(&chunk);
            }
            let response: Value =
                serde_json::from_slice(&bytes).context("Invalid cloud verification response")?;
            validate_completed_metadata(&response, &metadata, &id)?;
            Ok(())
        }
        .await;
        let (state, error) = match result {
            Ok(()) => ("complete", None),
            Err(error) => ("pending", Some(error.to_string())),
        };
        let persisted =
            tokio::task::spawn_blocking(move || manager.set_upload_status(&id, state, error)).await;
        if !matches!(persisted, Ok(Ok(_))) {
            tracing::error!("Could not persist recording upload status; local file retained");
        }
    });
    Ok(serde_json::to_value(status)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grants_bind_exact_path_headers_expiry_and_recording() {
        let expiration = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let mut grant: UploadGrant = serde_json::from_value(json!({"recordingId":"id","upload":{"url":"https://storage.googleapis.com/bucket/object?X-Goog-Signature=secret","method":"PUT","expiresAt":expiration,"headers":{"Content-Type":"application/vnd.sqlite3","x-goog-if-generation-match":"0","x-goog-content-length-range":"12,12"}},"completion":{"url":"/api/v3/data/device-recordings/room/room/uploads/id/complete","token":"secret","expiresAt":expiration}})).unwrap();
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "room", 12).is_ok());
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "other", "room", 12).is_err());
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "other", 12).is_err());
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "room", 13).is_err());
        grant
            .upload
            .as_mut()
            .unwrap()
            .headers
            .insert("Authorization".into(), "secret".into());
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "room", 12).is_err());
        grant.upload = None;
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "room", 12).is_ok());
        grant.completion.expires_at = "2020-01-01T00:00:00Z".into();
        assert!(validate_grant("https://dev.hyperstudy.io", &grant, "id", "room", 12).is_err());
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;
    #[test]
    fn completion_must_confirm_this_exact_artifact() {
        let metadata = json!({"sha256":"abc","fileSize":12});
        assert!(validate_completed_metadata(&json!({"data":[{"recordingId":"id","status":"complete","sha256":"abc","fileSize":12}]}),&metadata,"id").is_ok());
        assert!(validate_completed_metadata(&json!({"data":[{"recordingId":"other","status":"complete","sha256":"abc","fileSize":12}]}),&metadata,"id").is_err());
        assert!(validate_completed_metadata(&json!({"data":[{"recordingId":"id","status":"complete","sha256":"different","fileSize":12}]}),&metadata,"id").is_err());
        assert!(validate_completed_metadata(&json!({"success":true}), &metadata, "id").is_err());
    }
}
