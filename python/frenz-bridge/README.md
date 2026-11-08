# FRENZ hybrid sidecar

The SDK owns the single Bluetooth connection and continues computing its models.
A process-local, SDK-version-checked notification tap copies EEG bytes before
forwarding the original callback arguments unchanged. It does not patch the SDK
files, change model inputs, or establish a second connection. A valid Earable
product key is still required for this hybrid mode.

Selecting EEG in Bridge includes two separately named streams:

- `_EEG_bluetooth`: original packet data, before SDK windowing, at a nominal
  125 samples/second. Six raw signed 24-bit ADC channels; no claimed electrode
  names or microvolt conversion.
- `_EEG_raw`: the existing seven-column SDK-processed comparison stream. Its
  historical name is retained, but XML identifies it as processed comparison
  data rather than original packets.

`_focus` remains the unmodified SDK score history. Equal consecutive scores are
separate observations and are never deduplicated by value or timestamp.

## Raw EEG schema

`_EEG_bluetooth` contains 13 float64 columns, each lossless for the corresponding
wire integer. Order: original whole-second timestamp; six signed ADC values;
session-local packet index; 8-bit packet sequence; row index within the packet;
sample flags; packet trailer; host LSL callback-arrival time. Nine rows reconstruct
the complete 178-byte packet, including its fixed channel-count byte (six).

LSL timestamps equal the callback-arrival time, shared by all nine packet rows.
They are **not** calibrated sample acquisition times. The nominal rate, packet
order and coarse device clock remain available for later explicit reconstruction.
Sequence discontinuities and backwards device seconds are reported, not fixed
or silently removed. A new connection starts a new packet-index session.

Focus timestamps record the first Bridge observation of each SDK history row.
The SDK does not expose the model's input-window timing; this implementation does
not establish whether its internal buffers affect model accuracy.

## Failure and shutdown

The callback only copies bytes into a bounded queue; Rust's existing LSL recorder
provides SQLite durability downstream. An unsupported layout, queue overflow or
unsupported SDK version fails explicitly. The tap supports frenztoolkit 0.3.2
and the observed FRENZI85 layout; a matching packet length alone is not a firmware
compatibility certification. Stopping freezes capture and drains accepted packets.
Packets still in memory can be lost if the process is killed: no claim of zero
loss before the SQLite commit is made.

## Verification

Run `python -m unittest discover -s tests -v` from this directory with NumPy
installed. `build-pyapp.sh` includes all four sidecar modules in the wheel and
packages the runtime; the wheel version changes when deployed code changes.

November 7, 2026 local live check: 14,877 SQLite EEG rows reconstructed 1,653
consecutive packet payloads exactly, with six counter wraps and zero sequence
gaps. All 57 focus values matched the independent SDK history, including four
identical consecutive values. Both SQLite integrity checks passed; SDK shutdown
exited normally. This was a two-minute local recording, not a new cloud-upload
test, calibrated timing test, or unattended endurance guarantee.
