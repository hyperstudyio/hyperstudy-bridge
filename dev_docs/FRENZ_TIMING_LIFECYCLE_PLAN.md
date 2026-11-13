# FRENZ timing and lifecycle hardening

Authorized 2026-11-06: parallel implementation of faster connect/disconnect and
honest timestamp precision, followed by combined tests and one-device verification.

## Step 1 — Connection lifecycle
Open independent FRENZ inlets concurrently; require all requested streams or
surface failure and clean up. Cancel and join readers (including blocked senders),
close native resources deterministically, and measure timings. No automatic retries
or synchronization sleeps. Owner: connection_speed.

## Step 2 — Timestamp provenance and quality
Preserve every SDK source value and original timestamp. Investigate firmware versus
processed timing; record metadata and discontinuities without silently repairing
source clocks. Bound measurement of the local UNIX-to-LSL mapping. A synthesized
sample clock must never be labeled measured acquisition time. Owner: timestamps.

## Step 3 — Recording end and cleanup
Add explicit bounded per-stream timestamp targets for stop when requested. Persist
observational coverage, uncertainty/discontinuities, and timeout outcomes separately
from successful file finalization. Immediate stop remains explicit. Close native
inlets before destruction where supported; measure post-finalization cleanup.
Owner: recording_lifecycle.

## Step 4 — Sidecar exit and app status
Record a safe exit reason and broadcast disconnected status to all Bridge clients
when the sidecar exits. Clean up abandoned FRENZ inlets before permitting a new
sidecar start. Avoid leaving the UI Connected after a sleep/dropout. Owner: root.

## Step 5 — Integrated verification
Run relevant full suites, rebuild the packaged sidecar and native app, then measure
connection, recording, end-boundary outcomes, and disconnection with FRENZI85.
Retain original test recordings. No claim of precise absolute sensor/event alignment
without supporting device/SDK timing evidence. Production deploy/cloud upload are
outside this repair batch. Owner: root.


## Implemented and measured — 2026-11-06

Steps 1–4 are implemented. Step 5 includes combined automated tests and real FRENZI85
capture; results below distinguish source-time observations from acquisition accuracy.

- 216 Rust tests passed, 3 ignored; 221 frontend tests passed, 40 skipped; Svelte check
  zero errors/warnings; 12 Python tests passed. Initial transient local transport
  failures were retained in logs; a full rerun passed once transport recovered.
- Live setup of all 23 inlets: 832 ms, then 816 ms on warm reconnect. Warm end-to-end
  connect: 29.47 s (SDK import 6.92 s, initialization/BLE 7.36 s, first data 9.05 s,
  discovery and inlet setup 5.97 s). One first-run packaged-runtime install is separate.
- New bounded-stop API capture: 17,841 samples, 24 streams including markers,
  SQLite integrity/foreign keys/frame shapes/sequences verified. All five raw source
  boundaries observed after 3.222 s with no discontinuities in that capture; outcomes
  remain `observed_unverified`. Finalization 36 ms, inlet teardown 517 ms, total stop
  response 3.785 s. Earlier captures waited 12–14 s after finalization.
- Ordinary disconnect 2.864 s, including 1.301 s concurrent inlet cleanup. Controlled
  sidecar exit with no recording active changed UI to Disconnected/Error and cleaned
  all 23 inlets in 1.309 s. No process-exit cause is inferred from this injected test.
- A warm reconnect produced source-timestamp discontinuities; native UI displayed
  the warning as intended. Bracket measurement in the recording was 0.6045 microseconds,
  which is only host-read uncertainty and is NOT sensor acquisition accuracy.
- Focused review corrected native/API shutdown cleanup parity and one-stream
  disconnect acknowledgements. Success connect/disconnect modals were removed from
  DeviceCard; persistent state provides feedback, while errors remain visible.

Live evidence and audit scripts are in HyperStudy's
`tools/recording-bench/BRIDGE_LIVE_VALIDATION.md` and its ignored `device-recordings`
directory. No cloud upload, GitHub push, or deployment was performed. Absolute FRENZ
sample/event alignment still requires a validated vendor acquisition-clock model;
see [timestamp evidence](FRENZ_TIMESTAMP_EVIDENCE.md).


Final native UI verification: a complete connection displayed Connected/Streaming
without a success modal. API disconnect returned in 2.842 s and both native labels
changed to Disconnected/Stopped. The reviewed final Rust suite again passed 216 tests
(3 ignored), and the final frontend suite passed 221 tests (40 skipped), with zero
Svelte errors/warnings. The rebuilt development bundle includes these changes.
