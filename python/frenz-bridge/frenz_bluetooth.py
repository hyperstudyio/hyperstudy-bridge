"""One-connection EEG tap. Original SDK callback inputs remain unchanged.

Supported wire layout was verified against frenztoolkit 0.3.2 on FRENZI85.
Arrival time is an observation, not a calibrated sample acquisition timestamp.
"""

import inspect
import threading
from collections import deque

EEG_UUID = "45420201-0000-ffff-ff45-415241424c45"
EEG_SUFFIX = "_EEG_bluetooth"
EEG_COLUMNS = [
    ("source_timestamp", "seconds"),
    *[(f"adc_channel_{i}", "raw_ADC_counts") for i in range(6)],
    ("packet_index", "index"),
    ("packet_sequence", "uint8"),
    ("row_in_packet", "index"),
    ("sample_flags", "uint8"),
    ("packet_trailer", "uint8"),
    ("received_lsl_time", "seconds"),
]


def decode_eeg_rows(payload, packet_index, received):
    if len(payload) != 178 or payload[1] != 6:
        raise ValueError("Unsupported EEG packet layout")
    timestamp = int.from_bytes(payload[2:6], "big")
    rows = []
    for i in range(9):
        start = 6 + 19 * i
        values = [
            int.from_bytes(
                payload[start + 3 * j : start + 3 * j + 3], "big", signed=True
            )
            for j in range(6)
        ]
        rows.append(
            [
                timestamp,
                *values,
                packet_index,
                payload[0],
                i,
                payload[start + 18],
                payload[-1],
                received,
            ]
        )
    return rows


class EegPacketTap:
    def __init__(self, local_clock, queue_limit=4096):
        self.local_clock = local_clock
        self.queue_limit = queue_limit
        self.queue = deque()
        self.lock = threading.Lock()
        self.failure = None
        self.accepting = True
        self.captured_packets = 0
        self.sample_count = 0
        self.previous_sequence = None
        self.previous_seconds = None
        self.status = {
            "sample_count": 0,
            "packet_count": 0,
            "sequence_discontinuity_count": 0,
            "backward_count": 0,
            "last_sequence_discontinuity": None,
            "acquisition_alignment": "unverified",
        }
        self.client_class = None

    def capture(self, data):
        # No disk IO or vendor processing on this callback path. Copy mutable
        # payload bytes before handing the original object back to the SDK.
        try:
            received = self.local_clock()
            payload = bytes(data)
            with self.lock:
                if self.failure is not None or not self.accepting:
                    return
                if len(payload) != 178 or payload[1] != 6:
                    # Wire numbers only (no payload bytes), so the message stays safe to show and log.
                    kind = payload[1] if len(payload) > 1 else "none"
                    self.failure = (f"Unsupported EEG packet layout ({len(payload)} bytes, type {kind}, "
                                    f"after {self.captured_packets} packets)")
                elif len(self.queue) >= self.queue_limit:
                    self.failure = "EEG capture queue overflow"
                else:
                    self.queue.append((self.captured_packets, received, payload))
                    self.captured_packets += 1
        except Exception:  # SDK callback must still receive its original input.
            with self.lock:
                self.failure = "EEG notification capture failed"

    def raise_if_failed(self):
        with self.lock:
            if self.failure is not None:
                raise RuntimeError(self.failure)

    def finish_capture(self):
        """Freeze the recording boundary while the SDK finishes its own shutdown."""
        with self.lock:
            self.accepting = False

    def finish_and_drain(self, publish):
        self.finish_capture()
        # Always check failure, including a rejected packet with an empty queue.
        self.drain(publish)
        while self.timing_status()["queued_packets"]:
            self.drain(publish)

    def drain(self, publish):
        self.raise_if_failed()
        # Bound work per poll so model history publication cannot be starved.
        for _ in range(512):
            with self.lock:
                if not self.queue:
                    break
                index, received, payload = self.queue.popleft()
            rows = decode_eeg_rows(payload, index, received)
            publish(EEG_SUFFIX, rows, received, "packet_arrival")
            sequence, seconds = payload[0], rows[0][0]
            if (
                self.previous_sequence is not None
                and (sequence - self.previous_sequence) % 256 != 1
            ):
                self.status["sequence_discontinuity_count"] += 1
                self.status["last_sequence_discontinuity"] = {
                    "packet_index": index,
                    "previous": self.previous_sequence,
                    "current": sequence,
                }
            if self.previous_seconds is not None and seconds < self.previous_seconds:
                self.status["backward_count"] += 1
            self.previous_sequence, self.previous_seconds = sequence, seconds
            self.sample_count += 9
            self.status.update(sample_count=self.sample_count, packet_count=index + 1)
        self.raise_if_failed()

    def timing_status(self):
        with self.lock:
            return {
                **self.status,
                "queued_packets": len(self.queue),
                "captured_packets": self.captured_packets,
                "capture_closed": not self.accepting,
            }

    def install(self, client_class=None, sdk_version=None):
        if client_class is None:
            from bleak import BleakClient

            client_class = BleakClient
        if sdk_version is None:
            from importlib.metadata import version

            sdk_version = version("frenztoolkit")
        if sdk_version != "0.3.2":
            raise RuntimeError("Unsupported SDK version for EEG packet capture")
        if self.client_class is not None or getattr(
            client_class.start_notify, "_frenz_tap", False
        ):
            raise RuntimeError("EEG packet capture already installed")
        original = client_class.start_notify
        tap = self

        async def start_notify(client, characteristic, callback, *args, **kwargs):
            uid = str(getattr(characteristic, "uuid", characteristic)).lower()
            if uid == EEG_UUID:
                sdk_callback = callback
                if inspect.iscoroutinefunction(sdk_callback):

                    async def wrapped(sender, data):
                        tap.capture(data)
                        return await sdk_callback(sender, data)

                else:

                    def wrapped(sender, data):
                        tap.capture(data)
                        return sdk_callback(sender, data)

                callback = wrapped
            return await original(client, characteristic, callback, *args, **kwargs)

        start_notify._frenz_tap = True
        self.client_class = client_class
        self.original_notify = original
        self.wrapper = start_notify
        client_class.start_notify = start_notify

    def restore(self):
        if self.client_class is not None:
            if self.client_class.start_notify is not self.wrapper:
                raise RuntimeError("EEG notification hook changed unexpectedly")
            self.client_class.start_notify = self.original_notify
            self.client_class = None
