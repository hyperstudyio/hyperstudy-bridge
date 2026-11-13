# FRENZ timestamp provenance and limits

2026-11-06. Implements [timing/lifecycle plan, step 2](FRENZ_TIMING_LIFECYCLE_PLAN.md#step-2--timestamp-provenance-and-quality).

## What the clocks establish

The [vendor documentation for frenztoolkit 0.3.2](https://pypi.org/project/frenztoolkit/0.3.2/)
describes firmware arrays as original, uninterpolated packet data, available immediately
after packet receipt. It identifies column zero of processed RAW arrays as a timestamp,
but does not specify a validated device/host clock model, error bound, or alignment
between the processed samples, firmware samples and filtered/model outputs. The installed
macOS implementation is compiled CPython 3.9 extensions, not inspectable Python source.
The documentation and local recordings therefore do not justify correcting source time.

[LSL's clock documentation](https://labstreaminglayer.readthedocs.io/info/time_synchronization.html)
distinguishes the sender's monotonic clock from wall time and recommends preserving
clock observations for later synchronization. Its network clock correction does not
establish synchronization of an independently timed sensor to the sending computer.
Small local LSL correction uncertainty is not a bound on FRENZ acquisition alignment.

## Read-only evidence

Inputs: HyperStudy's `tools/recording-bench/BRIDGE_LIVE_VALIDATION.md` and
`tools/recording-bench/device-recordings/bridge-sqlite-live/a35f2022-668b-49bb-9b55-baaf9d514cb2.sqlite`.
The SQLite file was opened read-only and replayed through the new continuity detector,
block by block. The original timestamps and data were not modified.

| Processed RAW stream | Samples | Cadence deviations | Backward steps | Repeated times |
|---|---:|---:|---:|---:|
| EEG | 2,875 | 2 | 1 | 0 |
| PPG | 575 | 2 | 1 | 0 |
| IMU | 1,150 | 2 | 1 | 0 |
| HR | 23 | 2 | 0 | 1 |
| SpO2 | 23 | 2 | 0 | 1 |

EEG row 1,875 advances from 1793976502.992 to 1793976504.0; row 2,250
reverses from 1793976506.992 to 1793976506.0. All five firmware arrays have
zero backward steps. Firmware EEG has 2,637 rows, PPG 538 and IMU 1,100;
coarse seconds repeat within groups, and group sizes are not consistently nominal
sample rates. Consequently neither matching row indexes nor spreading rows uniformly
within seconds provides a measured acquisition clock. The discrepancy suggests SDK
processing/reconstruction but does not identify the vendor implementation's cause.

## Implemented behavior

- All 23 streams, source-ID format, typed samples, column zero and sample order remain.
  No timestamp smoothing, deduplication, synthetic acquisition times or discarded rows.
- Processed RAW keeps the fixed source-plus-host-clock mapping. One host wall-clock read
  is bracketed by LSL clock reads; midpoint and half-span are stored in XML metadata.
  The half-span measures only the read bracket, not wall-clock accuracy, drift or device
  clock error. There is no automatic remapping after an anomaly.
- Schema 3 distinguishes processed-source time, original firmware time and bridge poll
  observation time. Every stream declares `acquisition_alignment=unverified` and
  `clock_discontinuity_policy=preserve_and_report`. Only processed RAW advertises an
  applied source-to-LSL offset. Filtered/model histories and firmware still use the
  bridge's poll observation timestamp; equal timestamps are expected for batches.
- Sidecar streaming statuses include bounded per-stream counters and the latest anomaly
  (source row index, previous/current time, interval, observation time). Processed RAW
  flags deviations greater than max(1 microsecond, 1% of nominal interval); this is a
  diagnostic tolerance, not an acquisition-accuracy claim. Firmware repeats are counted
  but treated as expected; backward firmware time is anomalous. Counts are lifetime
  counts for this sidecar session, not automatically recording-specific counts.
- Source timing state advances only after a batch is accepted by the publisher. Failed
  publication remains fatal, with no retries of possibly partially published data.

The status summaries do not constitute an exhaustive persistent anomaly log. Source
columns retained in SQLite permit independent reconstruction; the recorder also needs
its own per-recording coverage diagnostics. First crossing of an end-marker timestamp
is observational evidence only: a subsequent backward source jump may invalidate it.
No acquisition-completeness claim is supported by that crossing alone.

## Validation

The Python suite passes 12 tests using the existing sidecar's CPython 3.9 environment,
including cross-batch +1/-1 second steps, low-rate repeated time, legitimate firmware
repetitions, failed-publication state, and clock bracket measurement. Real pylsl
`StreamInfo` serialization was checked for all 23 stream headers while replacing only
network outlet creation; source IDs were preserved. No hardware was used or restarted
for these checks. Native packaging/live validation are separate integration steps.
