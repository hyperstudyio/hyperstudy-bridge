# Managed study recording protocol

All commands use `{type:"command",device:"recording",action,payload,id}` and return
normal Bridge data/error responses. Payloads and capabilities are never logged.

- `discover_recording_streams`: no payload; returns resolved native LSL descriptors
  `{uid,name,source_id,channels,format,nominal_rate,xml}`.
- `start_study_recording`: `{stream_uids,context}`. Context requires
  `experiment_id,room_id,participant_id,session_id`; `label` is optional and is not
  part of ownership. Returns only after native inlets and SQLite are ready.
  Repeating the active identity attaches to it; another active identity is rejected.
- `get_study_recording`: `{context}`; returns active matching session, otherwise
  latest matching session, otherwise `null`.
- `send_study_marker`: `{recording_id,context,marker}`. Marker is JSON, at most
  64 KiB. Returns `{recording_id,marker_lsl_timestamp}` after durable commit.
- `stop_study_recording`: `{recording_id,context,end_marker?}`. An end marker is
  written once; duplicate stop returns the same final session. The marker is
  included in the automatic local-clock string stream `HyperStudy Study Markers`.
  Source clock mappings come from measured LSL corrections at most 10 seconds old.
  Native draining has an 8-second observation deadline; file synchronization and
  native teardown follow. Reports never claim verified acquisition coverage.
  Unmapped sources appear in `end_report.unmapped_streams`; timestamp anomalies,
  missing tails and timeouts remain explicit. Omitting end_marker is an immediate
  local-style stop with `acquisition_coverage:"not_requested"`.
- `prepare_recording_upload`: `{recording_id}`; validates finalized standalone
  SQLite, reads actual metadata/counts and hashes bytes using 128 KiB reads. Returns
  the API manifest with camelCase fields, capped at 1 GiB. Stream XML is omitted
  from the manifest but remains in the original SQLite file.
- `upload_study_recording`: `{recording_id,api_base_url,grant}`; validates the grant
  and starts an independent job. API base is an exact trusted HyperStudy HTTPS
  origin (localhost is permitted in debug builds only). GCS upload target must be
  `https://storage.googleapis.com`, with no redirects. The grant must contain
  `recordingId`, nullable `upload`, and `completion`; expiries are RFC3339 strings.
  A null upload performs completion-only verification. PUT forwards exactly the
  required Content-Type, generation-match and content-length-range headers.
- `list_study_recordings`: no payload; returns oldest-to-newest session inventory.

Session response fields: `recording_id,created_at,context,state,path,
committed_samples,error,streams,end_report,upload_status,upload_error`.
Upload states: `local`, `uploading`, `verifying`, `complete`, `pending`.
Failure retains the local artifact and records a sanitized error. No automatic
retry or file deletion occurs. Restart converts unfinished upload jobs to pending
and requires an explicit fresh grant. Capabilities exist only in memory.

Browser closure never stops acquisition or an already-enqueued upload. The native
RecordingPanel retains its explicit immediate stop. Legacy WebSocket stop cannot
bypass study identity checks. State sidecars live beside the SQLite files under
Tauri's app-data `recordings` directory; browser requests cannot select that path.
