# FRENZ SQLite acquisition implementation plan

Goal: Correct FRENZ acquisition and add independent, durable local LSL recording.
Architecture: Python publishes incremental typed SDK streams. A Rust-owned LSL
reader and SQLite writer run independently of UI/browser consumers. A recording
file owns its stream metadata, binary samples, original LSL timestamps, local
receive times, clock observations and recording context. No credential enters it.
Spec: User-approved SQLite decision and capture evidence in HyperStudy's
`tools/recording-bench/LIVE_RESULTS.md`; this is the next local-acquisition batch.
Cloud upload/API integration follows this batch and is not implied by local success.

Constraints: no silent fallback, no automatic reconnection/retry, no sample
resampling/deduplication by timestamp; FULL WAL + fullfsync, explicit errors,
exclusive new files, recover interrupted files without overwriting originals.
Use the existing Svelte 5/Tauri application. Avoid unrelated fixes/deployment.

Review focus: repeated firmware timestamps; growing/shrinking buffers; writer
failure or disk exhaustion; stop racing sample acquisition; lost LSL sources;
credential leakage; empty streams falsely reported as streaming.

## Task 1: FRENZ publisher
- [x] Test incremental cursors, repeated timestamps, filtered transpose, model
  histories, shape failures and failed publication without cursor advancement.
- [x] Implement a data-only `frenz_streams.py` with versioned stream definitions
  and StreamPublisher.poll(streamer, emit_chunk, observed_time).
- [x] Fix startup directory and readiness; surface failure rather than continuing.
- [x] Pin Python 3.9 / SDK 0.3.2 in package/build workflows; include helper module.
- [x] Update resolver suffixes, configuration fields and channel labels.
- [x] Verify Python tests and real recorded fixture replay through publisher.

## Task 2: Durable generic recorder
- [x] Test SQLite exclusive creation, exact typed binary encoding, transactional
  counters, interrupted reopen, invalid shapes and failed writes.
- [x] Implement `recording` module with `RecordingManager` start/stop/status,
  `RecordingRequest` (stream UIDs, output directory, typed experiment context),
  `RecordingStatus` (state, path, committed counts, error).
- [x] Single worker owns independent native LSL inlets and SQLite connection;
  batch up to 4096 samples / 100ms, preserve typed data and raw timestamps.
  Store full stream XML and clock correction observations. On failure preserve
  committed data and surface error; on stop finalize, checkpoint and sync.
- [x] Expose native and WebSocket list/start/stop/status interfaces. Recovery
  finalizes a copied interrupted database, with an explicit interrupted state.
- [x] Verify real LSL outlet -> recorder -> SQLite rows and stop/failure behavior.

## Task 3: Controls and verification
- [x] Add a recording panel: explicit stream selection, folder, start/stop,
  durable sample counts, failure status and saved path. Existing credential vault
  remains the key store; device ID becomes editable for initial BLE connection.
- [x] Test UI commands and configuration. Run Python, Rust, frontend suites,
  frontend build/type checks. Report baseline failures separately.
- [x] Independent final review, fix findings, document schema and commands.
- [x] Leave reviewed local branch available; no publish or deployment.
