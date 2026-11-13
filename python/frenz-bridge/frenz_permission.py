"""Wait for macOS to answer the Bluetooth permission prompt before bleak starts.

bleak creates its CBCentralManager and waits one second for the first state update
(bleak/backends/corebluetooth/CentralManagerDelegate.py). While macOS is showing the permission prompt no update
arrives, so bleak raises "Bluetooth device is turned off" and the first connect or scan after every prompt failed
(the prompt reappears for each new ad-hoc-signed Bridge build). Here the sidecar asks first and waits for the answer.
"""
import functools
import sys
import threading

# CBManagerAuthorization
NOT_DETERMINED, RESTRICTED, DENIED, ALLOWED = 0, 1, 2, 3

DENIED_MESSAGE = ('Bluetooth access is off for HyperStudy Bridge. Turn it on in System Settings > Privacy & '
                  'Security > Bluetooth, then connect again.')


class BluetoothPermissionError(RuntimeError):
    """Fixed, credential-free message for a refused Bluetooth permission."""


def await_bluetooth_permission(stopping, on_prompt, authorization=None, open_manager=None):
    """Return True once Bluetooth may be used (asking first if macOS has not yet), False if `stopping` was set
    while the prompt was open. Raises BluetoothPermissionError when access is refused.

    `authorization()` reads the current CBManagerAuthorization; `open_manager(answered)` creates a central manager,
    which shows the prompt, and sets `answered` whenever its state updates. Both default to CoreBluetooth.
    """
    if authorization is None:
        if sys.platform != 'darwin':
            return True
        authorization, open_manager = _corebluetooth()
    if authorization() == NOT_DETERMINED:
        on_prompt()
        answered = threading.Event()
        manager = open_manager(answered)  # keep a reference: the prompt belongs to this manager
        while authorization() == NOT_DETERMINED:
            # The state update is the answer; the wait also yields to a stop request.
            if answered.wait(0.25):
                answered.clear()
            if stopping.is_set():
                return False
        del manager
    if authorization() in (RESTRICTED, DENIED):
        raise BluetoothPermissionError(DENIED_MESSAGE)
    return True


@functools.lru_cache(maxsize=None)  # an Objective-C class can be defined only once per process
def _corebluetooth():
    from CoreBluetooth import CBCentralManager
    from Foundation import NSObject
    from libdispatch import dispatch_queue_create, DISPATCH_QUEUE_SERIAL

    class _AnswerDelegate(NSObject):
        def centralManagerDidUpdateState_(self, _manager):
            self.answered.set()

    def open_manager(answered):
        delegate = _AnswerDelegate.alloc().init()
        delegate.answered = answered
        queue = dispatch_queue_create(b'hyperstudy.bluetooth-permission', DISPATCH_QUEUE_SERIAL)
        return delegate, CBCentralManager.alloc().initWithDelegate_queue_(delegate, queue)

    return CBCentralManager.authorization, open_manager
