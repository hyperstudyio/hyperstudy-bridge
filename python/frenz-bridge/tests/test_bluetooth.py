import unittest

from frenz_bluetooth import EEG_UUID, EegPacketTap, decode_eeg_rows


def packet(sequence=0, timestamp=1234):
    values = [-8388608, -1, 0, 1, 8388607, 42]
    row = b"".join(value.to_bytes(3, "big", signed=True) for value in values) + b"\x03"
    return bytes([sequence, 6]) + timestamp.to_bytes(4, "big") + row * 9 + b"\x05"


class PacketTests(unittest.TestCase):
    def test_stop_surfaces_capture_failure_even_with_an_empty_queue(self):
        tap = EegPacketTap(lambda: 42.)
        tap.capture(b'bad')
        self.assertEqual(tap.timing_status()['queued_packets'], 0)
        with self.assertRaisesRegex(RuntimeError, 'layout'):
            tap.finish_and_drain(lambda *args: None)

    def test_stop_boundary_drains_previously_captured_packets_only(self):
        tap = EegPacketTap(lambda: 42.0)
        tap.capture(packet(0))
        tap.finish_capture()
        tap.capture(packet(1))
        batches = []
        tap.drain(lambda *args: batches.append(args))
        self.assertEqual(tap.sample_count, 9)
        self.assertEqual(tap.timing_status()["queued_packets"], 0)
        self.assertTrue(tap.timing_status()["capture_closed"])

    def test_rows_round_trip_every_wire_byte(self):
        payload = packet(255)
        rows = decode_eeg_rows(payload, 7, 42.5)
        self.assertEqual(len(rows), 9)
        self.assertEqual(
            rows[0], [1234, -8388608, -1, 0, 1, 8388607, 42, 7, 255, 0, 3, 5, 42.5]
        )
        rebuilt = bytes([int(rows[0][8]), 6]) + int(rows[0][0]).to_bytes(4, "big")
        for i, row in enumerate(rows):
            self.assertEqual(row[9], i)
            rebuilt += b"".join(
                int(v).to_bytes(3, "big", signed=True) for v in row[1:7]
            ) + bytes([int(row[10])])
        rebuilt += bytes([int(rows[0][11])])
        self.assertEqual(rebuilt, payload)

    def test_sequence_wrap_repeated_values_and_clock_reversals_are_preserved(self):
        tap = EegPacketTap(lambda: 42.0)
        for seq, timestamp in [(254, 10), (255, 10), (0, 9), (2, 9), (2, 9)]:
            tap.capture(packet(seq, timestamp))
        batches = []
        tap.drain(lambda *args: batches.append(args))
        self.assertEqual(tap.sample_count, 45)
        self.assertEqual(len(batches), 5)
        self.assertEqual(tap.timing_status()["sequence_discontinuity_count"], 2)
        self.assertEqual(tap.timing_status()["backward_count"], 1)
        self.assertEqual(batches[2][1][0][0], 9)


class TapTests(unittest.IsolatedAsyncioTestCase):
    async def test_sdk_receives_identical_objects_once_and_other_streams_are_untouched(
        self,
    ):
        class Client:
            async def start_notify(self, characteristic, callback):
                self.callback = callback

        original = Client.start_notify
        tap = EegPacketTap(lambda: 42.0)
        tap.install(Client, "0.3.2")
        try:
            client = Client()
            seen = []
            callback = lambda sender, data: seen.append((sender, data))
            await client.start_notify(EEG_UUID, callback)
            data = bytearray(packet())
            client.callback("sender", data)
            self.assertEqual(len(seen), 1)
            self.assertIs(seen[0][1], data)
            data[6] = 0
            batches = []
            tap.drain(lambda *args: batches.append(args))
            self.assertEqual(batches[0][1][0][1], -8388608)
            await client.start_notify("another-stream", callback)
            self.assertIs(client.callback, callback)
        finally:
            tap.restore()
        self.assertIs(Client.start_notify, original)

    async def test_async_sdk_callback_is_awaited_even_when_queue_overflows(self):
        class Client:
            async def start_notify(self, characteristic, callback):
                self.callback = callback

        tap = EegPacketTap(lambda: 42.0, queue_limit=1)
        tap.install(Client, "0.3.2")
        try:
            seen = []

            async def callback(sender, data):
                seen.append(data)

            client = Client()
            await client.start_notify(EEG_UUID, callback)
            await client.callback(None, packet(0))
            await client.callback(None, packet(1))
            self.assertEqual(len(seen), 2)
            with self.assertRaisesRegex(RuntimeError, "overflow"):
                tap.drain(lambda *args: None)
        finally:
            tap.restore()

    async def test_malformed_eeg_still_reaches_sdk_and_capture_fails_explicitly(self):
        class Client:
            async def start_notify(self, characteristic, callback):
                self.callback = callback

        tap = EegPacketTap(lambda: 42.0)
        tap.install(Client, "0.3.2")
        try:
            client = Client()
            seen = []
            await client.start_notify(EEG_UUID, lambda sender, data: seen.append(data))
            data = bytearray(b"bad")
            client.callback(None, data)
            self.assertIs(seen[0], data)
            with self.assertRaisesRegex(RuntimeError, "layout"):
                tap.drain(lambda *args: None)
        finally:
            tap.restore()

    def test_layout_failure_describes_the_rejected_packet(self):
        # The band intermittently sends a notification that is not a 178-byte type-6 EEG frame ~0.6 s into streaming.
        # The fixed message alone could not say what it was; its size, type byte and position are wire numbers only.
        tap = EegPacketTap(lambda: 42.0)
        tap.capture(packet(0))
        tap.capture(packet(1))
        tap.capture(bytes([7, 9, 1, 2]))
        with self.assertRaisesRegex(
            RuntimeError, r"^Unsupported EEG packet layout \(4 bytes, type 9, after 2 packets\)$"
        ):
            tap.drain(lambda *args: None)
        empty = EegPacketTap(lambda: 42.0)
        empty.capture(b"")
        with self.assertRaisesRegex(RuntimeError, r"\(0 bytes, type none, after 0 packets\)$"):
            empty.drain(lambda *args: None)

    async def test_second_tap_is_rejected_and_sdk_exceptions_propagate(self):
        class Client:
            async def start_notify(self, characteristic, callback):
                self.callback = callback

        tap = EegPacketTap(lambda: 42.0)
        tap.install(Client, "0.3.2")
        try:
            with self.assertRaisesRegex(RuntimeError, "already installed"):
                EegPacketTap(lambda: 42.0).install(Client, "0.3.2")

            def fail(sender, data):
                raise ValueError("SDK callback failure")

            client = Client()
            await client.start_notify(EEG_UUID, fail)
            with self.assertRaisesRegex(ValueError, "SDK callback failure"):
                client.callback(None, packet())
            self.assertEqual(tap.captured_packets, 1)
        finally:
            tap.restore()

    async def test_unknown_sdk_version_does_not_install_hook(self):
        class Client:
            async def start_notify(self, characteristic, callback):
                pass

        original = Client.start_notify
        with self.assertRaisesRegex(RuntimeError, "version"):
            EegPacketTap(lambda: 42.0).install(Client, "99.0")
        self.assertIs(Client.start_notify, original)


if __name__ == "__main__":
    unittest.main()
