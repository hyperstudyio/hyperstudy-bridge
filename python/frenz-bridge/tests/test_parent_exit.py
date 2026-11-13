"""The sidecar releases the band when Bridge goes away.

When Bridge quit, the sidecar saw stdin close and left its loop, but its first cleanup step reported 'stopping' to
Bridge's closed stdout. The BrokenPipeError skipped streamer.stop(), the SDK's non-daemon Bluetooth threads kept
running, and the process stayed connected to the band, so no later Bridge could connect to it.

Runs the real main() in a child process over real pipes; only the vendor SDK, LSL and the packet tap are faked. The
fake SDK, like the real one, streams on a non-daemon thread that ends only on stop().
"""
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent

CHILD = textwrap.dedent('''
    import os, sys, threading, types
    sys.path.insert(0, sys.argv[1])
    stopped_file = sys.argv[2]

    class Streamer:
        def __init__(self, **_):
            self._done = threading.Event()
        def start(self):
            threading.Thread(target=self._done.wait).start()   # non-daemon, like the SDK's BLE threads
        def stop(self):
            open(stopped_file, 'w').close()
            self._done.set()

    sys.modules['frenztoolkit'] = types.SimpleNamespace(Streamer=Streamer)
    sys.modules['pylsl'] = types.SimpleNamespace(local_clock=lambda: 0.0)

    import frenz_lsl_bridge as bridge

    class Tap:
        sample_count = 0
        failure = None
        def __init__(self, _clock): pass
        def install(self): pass
        def drain(self, _publish): pass
        def finish_capture(self): pass
        def finish_and_drain(self, _publish): pass
        def restore(self): pass
        def timing_status(self): return {}

    class Publisher:
        cursors = {}
        sample_count = 0
        def poll(self, *_): pass
        def timing_status(self): return {}

    bridge.EegPacketTap = Tap
    bridge.StreamPublisher = Publisher
    bridge.create_outlets = lambda *_: {}
    bridge.measure_clock_pair = lambda *_: {'offset_seconds': 0.0}
    bridge.prepare_lsl_runtime = lambda: None
    bridge.await_bluetooth_permission = lambda *_: True  # the host's real permission is not under test here
    bridge.main()
''')


class ParentExitTests(unittest.TestCase):
    def test_bridge_quitting_mid_stream_stops_the_sdk_and_ends_the_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            stopped = os.path.join(tmp, 'stopped')
            child = subprocess.Popen([sys.executable, '-c', CHILD, str(HERE), stopped],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                child.stdin.write(b'{"device_id": "FRENZTEST", "product_key": "k"}\n')
                child.stdin.flush()
                for line in child.stdout:
                    if b'waiting_for_data' in line:
                        break
                # Bridge quits: both of its pipe ends close at once.
                child.stdout.close()
                child.stdin.close()
                child.wait(timeout=10)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
            self.assertTrue(os.path.exists(stopped), 'the SDK was never stopped, so the band stays connected')


if __name__ == '__main__':
    unittest.main()
