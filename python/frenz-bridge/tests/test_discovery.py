import unittest
from types import SimpleNamespace as Obj
from frenz_discovery import select_bands, scan_error

class DiscoveryTests(unittest.TestCase):
    def test_only_advertised_frenz_names_and_distinct_addresses(self):
        rows={'a':(Obj(address='a',name='cached'),Obj(local_name='FRENZI85',rssi=-45)),
              'b':(Obj(address='b',name='FRENZ_stale'),Obj(local_name=None,rssi=-80)),
              'c':(Obj(address='c',name='headphones'),Obj(local_name='Headphones',rssi=-20))}
        self.assertEqual(select_bands(rows),[{'name':'FRENZI85','address':'a','rssi':-45,'selectable':True}])
    def test_duplicate_names_are_not_silently_selected(self):
        rows={k:(Obj(address=k),Obj(local_name='FRENZI85',rssi=-40)) for k in ('a','b')}
        self.assertTrue(all(not b['selectable'] for b in select_bands(rows)))
    def test_errors_are_actionable_without_arbitrary_exception_text(self):
        self.assertIn('Turn on Bluetooth',scan_error(Exception('Bluetooth device is turned off')))
        self.assertIn('permission',scan_error(Exception('Bluetooth access is denied')))
        self.assertNotIn('secret',scan_error(Exception('secret')))
