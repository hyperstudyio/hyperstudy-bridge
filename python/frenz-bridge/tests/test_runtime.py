import unittest
from unittest.mock import patch

import frenz_lsl_bridge as bridge


class RuntimeTests(unittest.TestCase):
    @unittest.skipIf(bridge.sys.platform == 'win32', 'Unix resource limits')
    def test_gui_soft_limit_is_raised_without_changing_hard_limit(self):
        import resource
        with patch.object(resource, 'getrlimit', return_value=(256, resource.RLIM_INFINITY)), \
             patch.object(resource, 'setrlimit') as set_limit:
            bridge.prepare_lsl_runtime()
        set_limit.assert_called_once_with(resource.RLIMIT_NOFILE, (1024, resource.RLIM_INFINITY))

    @unittest.skipIf(bridge.sys.platform == 'win32', 'Unix resource limits')
    def test_existing_high_limit_is_preserved(self):
        import resource
        with patch.object(resource, 'getrlimit', return_value=(4096, 8192)), \
             patch.object(resource, 'setrlimit') as set_limit:
            bridge.prepare_lsl_runtime()
        set_limit.assert_not_called()

    @unittest.skipIf(bridge.sys.platform == 'win32', 'Unix resource limits')
    def test_low_hard_limit_fails_explicitly(self):
        import resource
        with patch.object(resource, 'getrlimit', return_value=(256, 512)), \
             patch.object(resource, 'setrlimit') as set_limit:
            with self.assertRaisesRegex(RuntimeError, '1024'):
                bridge.prepare_lsl_runtime()
        set_limit.assert_not_called()

    @unittest.skipIf(bridge.sys.platform == 'win32', 'Unix resource limits')
    def test_permission_failure_has_fixed_safe_error(self):
        import resource
        with patch.object(resource, 'getrlimit', return_value=(256, 4096)), \
             patch.object(resource, 'setrlimit', side_effect=OSError('arbitrary system detail')):
            with self.assertRaisesRegex(bridge.StreamResourceError,
                                        '^Unable to reserve file/socket capacity for FRENZ streams.$'):
                bridge.prepare_lsl_runtime()

    def test_windows_does_not_import_unix_resource_module(self):
        with patch.object(bridge.sys, 'platform', 'win32'):
            bridge.prepare_lsl_runtime()


if __name__ == '__main__':
    unittest.main()
