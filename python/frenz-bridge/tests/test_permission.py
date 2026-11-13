"""The first connect or scan after macOS's Bluetooth prompt failed: bleak waits one second for CoreBluetooth's first
state update, and none arrives while the prompt is open. The sidecar now waits for the answer before bleak starts.

CoreBluetooth is replaced by a fake whose prompt is answered from another thread, as macOS answers it.
"""
import threading
import unittest

from frenz_permission import (ALLOWED, DENIED, NOT_DETERMINED, RESTRICTED, BluetoothPermissionError,
                              await_bluetooth_permission)


class FakeMacOS:
    def __init__(self, status, answer=None, answer_after=0.05):
        self.status, self.answer, self.answer_after = status, answer, answer_after
        self.prompts = 0

    def authorization(self):
        return self.status

    def open_manager(self, answered):
        self.prompts += 1
        if self.answer is not None:
            def respond():
                self.status = self.answer
                answered.set()
            threading.Timer(self.answer_after, respond).start()
        return object()


class PermissionTests(unittest.TestCase):
    def wait(self, macos, stopping=None):
        prompted = []
        result = await_bluetooth_permission(stopping or threading.Event(), lambda: prompted.append(True),
                                            macos.authorization, macos.open_manager)
        return result, bool(prompted)

    def test_already_allowed_never_prompts(self):
        macos = FakeMacOS(ALLOWED)
        self.assertEqual(self.wait(macos), (True, False))
        self.assertEqual(macos.prompts, 0)

    def test_waits_for_the_answer_however_long_it_takes(self):
        macos = FakeMacOS(NOT_DETERMINED, answer=ALLOWED, answer_after=1.5)  # longer than bleak's 1 s
        self.assertEqual(self.wait(macos), (True, True))
        self.assertEqual(macos.prompts, 1)

    def test_refusal_says_where_to_turn_it_on(self):
        for refused in (DENIED, RESTRICTED):
            with self.assertRaisesRegex(BluetoothPermissionError, 'Privacy & Security > Bluetooth'):
                self.wait(FakeMacOS(refused))
        with self.assertRaises(BluetoothPermissionError):
            self.wait(FakeMacOS(NOT_DETERMINED, answer=DENIED))

    def test_a_stop_while_the_prompt_is_open_ends_the_wait(self):
        stopping = threading.Event()
        threading.Timer(0.1, stopping.set).start()
        self.assertEqual(self.wait(FakeMacOS(NOT_DETERMINED), stopping), (False, True))


if __name__ == '__main__':
    unittest.main()
