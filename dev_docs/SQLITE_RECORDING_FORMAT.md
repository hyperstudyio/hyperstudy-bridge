# Local LSL recordings (schema 1)

SQLite is the selected acquisition format. This batch records locally; cloud
upload, Data API listing/download, Python SDK access and automatic experiment
lifecycle integration remain a separate batch. Existing FRENZ product-key entry
uses the credential vault. The band name is now editable before connecting.

## Use

Open **Recordings**, discover streams, select the exact stream instances, choose
an output folder, and start. Include the experiment marker stream in the selection
when needed. The panel can close while recording continues. Stop finalizes a
standalone UUID-named `.sqlite` file. Normal app quit also stops and finalizes.
Never delete an interrupted file's `-wal` or `-shm` siblings. Use **Recover file**
to create a separate `recovered-UUID.sqlite`; keep the originals until verified.
Do not copy only the main database while a recording is active.

The sidecar advertises 23 typed streams: processed EEG/PPG/IMU/HR/SPO2; filtered
EEG/EOG/EMG; focus, sleep stage, POAS, posture, signal quality and five power bands;
and five firmware streams. Availability depends on actual SDK output. No distinct
cognitive-load output was observed in the tested SDK. Preview selection does not
limit sidecar publication; the recording panel controls which streams are saved.

## Durability and limitations

A single worker owns independent LSL inlets and a SQLite connection. Transactions
commit at 100 ms or 4096 rows, with WAL, synchronous=FULL, fullfsync and
checkpoint_fullfsync. Counters advance only after successful commits. Stop drains
the bounded queue already in each inlet, commits, consolidates WAL and writes the
completed marker only after successful consolidation. Errors stop acquisition
without an automatic retry/reconnection and preserve previously committed rows.
Recovery uses SQLite backup into a new file and marks it interrupted.

Uncommitted buffers can be lost on process termination. The 100 ms target is not
a hard loss bound during stalls. Hardware/storage failures and upstream Bluetooth,
SDK or LSL loss cannot be made impossible. Sequence numbers describe received
rows, not device packet continuity. Selected streams may remain empty; zero rows
must not be interpreted as a successful device recording. Windows int64 acquisition
is explicitly rejected by the current Rust LSL binding; this batch was tested on Mac.
The SDK maintains growing histories; long-session SDK memory/copy overhead still
needs measurement. Recovery requires enough free space for a second database.

## Schema and encoding

`PRAGMA application_id = 0x48535231`, `user_version = 1`.

- `recording`: one row with state, UTC start/end, durable sample count, error and
  typed context JSON (experiment/room/participant/session IDs and label).
- `streams`: local stream ID and descriptor JSON with LSL UID, source ID, name,
  channel count, format, nominal rate and full source XML.
- `blocks`: stream ID, first_sequence (zero based), sample_count, timestamps,
  received, data. Read in per-stream first_sequence order.
- `clock_observations`: stream, observation local LSL time, remote LSL time,
  correction, uncertainty, or explicit estimation error.

`timestamps` and `received` are sample_count little-endian float64 values.
`data` contains sample_count frames: a little-endian uint32 byte length followed
by that sample's channel values. Numeric channels use the stream's little-endian
float32/float64/int8/int16/int32/int64 type. String frames are UTF-8 JSON arrays.
Original float bits and int64 precision are retained; equal timestamps are valid.
No interpolation, resampling, timestamp correction or deduplication is applied.

### Timestamps

Every time is LSL time in seconds (`lsl::local_clock`), but not all from the
same clock:

- `blocks.timestamps`: the time the source put on each sample, exactly as its
  inlet delivered it (`pull_sample`, no postprocessing). It is in the **source
  machine's** LSL clock. This is the sample time to analyze.
- `blocks.received`: Bridge's LSL clock when it pulled the sample. It is
  auxiliary evidence of delivery delay and gaps, never a sample time.
- Study marker stream (`HyperStudy Study Markers`): `timestamps` and `received`
  are both Bridge's LSL clock at the moment HyperStudy's marker was written.
- `clock_observations`: every 5 s per stream Bridge reads LSL's latest
  background measurement of (Bridge clock − source clock): `correction`, with
  its `uncertainty` and the source time it was measured at (`remote_lsl`).
  `observed_lsl` is Bridge's clock when it was read. A failed read records its
  error instead.

To put a source's samples on the marker timeline, add that stream's
`correction` (interpolated across observations) to its `timestamps`. For a
source on the same machine as Bridge the correction is about 0. Do not use
`received` for alignment: it includes Bridge's polling and transport delay.

Processed FRENZ raw streams preserve the SDK UNIX timestamp as channel 0 and use
a session-fixed UNIX-to-LSL offset for their LSL timestamps. Firmware channel 0
is preserved too, including repeated timestamps. Filtered/model/firmware streams
use explicit observation timestamps because exact per-row acquisition times are
not exposed in those SDK stores. They must not be treated as sample-accurately
aligned without further reconstruction/validation. XML records this policy and
column indices; unspecified units/electrode identities are not invented.

## Interfaces

Native commands: `discover_recording_streams`, `start_lsl_recording`,
`stop_lsl_recording`, `lsl_recording_status`, `recover_lsl_recording`.
Start takes `{request: {stream_uids, directory, context}}`; recover takes `{path}`.
WebSocket actions: `discover_recording_streams`, `start_lsl_recording`,
`stop_lsl_recording`, `lsl_recording_status`. Start's `payload` contains the
RecordingRequest directly (command `device` is `recording`). Responses use the `recording` data target.

FRENZ browser preview messages add `data: {format, encoding, values}`.
Numeric values use `encoding: "base64-le"` with original sample bytes; strings
use `encoding: "json"` and an array. Legacy `values` remains float-only preview.
Clients must decode `data` for integer streams, rather than parse int64 as a
JavaScript Number. The independent SQLite recorder does not use preview messages.

## Remaining integration

Build/test the packaged Python sidecar and run a fresh band-to-LSL-to-SQLite
session; measure long-run load. Add interrupted acquisition and source-loss soak
tests. Next cloud batch needs authorized experiment/participant association,
completed-file checksums, resumable upload with explicit state, server validation
and registration, Data API/SDK access, retention and local deletion policy.
A failed upload must leave the local recording intact. Test both platform runner
lifecycles when connecting automatic recording to experiment start/stop.
