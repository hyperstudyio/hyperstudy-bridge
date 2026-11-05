# Implementation evidence — FRENZ_SQLITE_PLAN.md

Base: 202993c; isolated branch codex/frenz-sqlite-recording.
Scope: local acquisition/SQLite batch; cloud/API follows. No deployment.

## Verification (2026-11-05, Apple Silicon macOS)

- Baseline: 192 Rust tests passed (2 ignored); 203 frontend tests (40 skipped).
- Final library suite: 200 Rust tests passed (2 ignored). Includes actual native
  LSL int64 input, equal timestamps, exact bytes, disk-full transaction failure,
  checkpoint failure, bounded stop drain, and integer WebSocket encoding.
- Frontend: 206 tests passed (40 skipped); Svelte check 0 errors/0 warnings;
  production Vite build and isolated Tauri debug app bundle passed.
- Python: 4 regression tests passed. Saved real capture replay through the new
  publisher produced 23 streams / 85,683 rows; second poll added zero duplicates.
- Actual pylsl smoke test created all 23 outlets with versioned metadata and
  int64 firmware format. No SDK/device connection during this implementation.
- Wheel build verified both Python modules. Full PyApp executable packaging,
  fresh device acquisition and Windows execution remain unverified.
- Independent review found checkpoint/stop/quit/preview integer issues; fixes
  received focused re-review. Review was by inspection, separate from tests.

## Native UI evidence

Verified the actual isolated desktop app, not mocked browser controls:
- Find streams, select synthetic source, choose a folder by text and start.
- Counter advanced with real SQLite commits. Closing/reopening the panel kept
  acquisition running (252 -> 385 saved samples).
- First Cmd+Q test exposed macOS predefined Quit bypassing ExitRequested.
  Replaced the menu with a controlled exit action; added a synchronous Exit
  flush for OS termination. Repeated Cmd+Q finalized 114 sequential samples in
  a standalone DELETE-journal database, integrity_check=ok, exact values.
- New local WebSocket discover/start commands drove the same native manager;
  the native panel displayed the recording started through WebSocket.
- Recovery chooser created a copy of the earlier interrupted file. Every block,
  stream descriptor and clock observation matched; original state stayed
  recording, copy state interrupted, integrity_check=ok.
- Stop and save button finalized another recording with 245 samples.

Synthetic artifacts: /private/tmp/hyperstudy-recording-ui-validation (not tracked).
Test app bundle: src-tauri/target/debug/bundle/macos/HyperStudy Bridge Recording Dev.app
The installed application was not replaced. The isolated test app was closed.

## Decisions and boundaries

Publisher cursors count SDK rows, never timestamps. Independent recorder inlets
avoid coupling persistence to UI/browser queues. Firmware and integer models
retain int64 precision. Clock metadata records source timestamps separately from
observations; filtered/model acquisition timing is not invented. See
SQLITE_RECORDING_FORMAT.md for encoding, recovery and outstanding integration.

The real backlog test initially exposed a synthetic producer handshake race;
waiting for LSL's consumer signal fixed the test without guessed startup sleeps.
Sandbox-denied local networking/cache writes were rerun with approved access.
Current code has not undergone a long-run resource/packet-loss soak test.
