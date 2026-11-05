import unittest
from types import SimpleNamespace
import numpy as np
from frenz_streams import StreamPublisher, STREAMS

class PublisherTests(unittest.TestCase):
    def test_incremental_arrays_preserve_equal_timestamps_and_integer_precision(self):
        p=StreamPublisher(); sent=[]
        s=SimpleNamespace(DATA={'RAW':{},'FILTERED':{}}, SCORES={})
        data=np.array([[1700000000,2**54],[1700000000,2**54+1]],dtype=np.int64)
        # HR is timestamp + value; even equal timestamps represent distinct rows.
        s.get_raw_data_realtime=lambda: {'hr':data}
        p.poll(s,lambda *args:sent.append(args),42.)
        p.poll(s,lambda *args:sent.append(args),43.)
        self.assertEqual(len(sent),1)
        self.assertEqual(sent[0][0],'_HR_firmware')
        self.assertEqual(sent[0][1].dtype,np.dtype('int64'))
        self.assertEqual(sent[0][1].tobytes(),data.tobytes())
        self.assertEqual(p.sample_count,2)
    def test_filtered_axes_and_model_histories(self):
        p=StreamPublisher();sent=[]
        s=SimpleNamespace(DATA={'RAW':{},'FILTERED':{'EEG':np.arange(12.).reshape(4,3)}},
            SCORES={'array__sqc_scores':[[1,0,1,0],[0,1,0,1]], 'array__posture':['left','left']},
            get_raw_data_realtime=lambda:{})
        p.poll(s,lambda *args:sent.append(args),42.)
        by_name={x[0]:x[1] for x in sent}
        np.testing.assert_array_equal(by_name['_EEG_filtered'],np.arange(12.).reshape(4,3).T)
        self.assertEqual(by_name['_signal_quality'].shape,(2,4))
        self.assertEqual(by_name['_POSTURE'],[['left'],['left']])
    def test_failed_push_does_not_advance_cursor_and_shrink_is_explicit(self):
        p=StreamPublisher()
        s=SimpleNamespace(DATA={'RAW':{'HR':np.array([[1.,60.],[2.,60.]])},'FILTERED':{}},
                          SCORES={},get_raw_data_realtime=lambda:{})
        def fail(*args):raise OSError('write failed')
        with self.assertRaises(OSError):p.poll(s,fail,42.)
        self.assertEqual(p.sample_count,0)
        p.poll(s,lambda *args:None,42.)
        s.DATA['RAW']['HR']=np.array([[1.,60.]])
        with self.assertRaisesRegex(ValueError,'shrank'):p.poll(s,lambda *args:None,43.)
    def test_empty_arrays_do_not_count_as_streaming_and_wrong_channels_fail(self):
        p=StreamPublisher()
        s=SimpleNamespace(DATA={'RAW':{'EEG':np.empty((0,7))},'FILTERED':{}},
                          SCORES={},get_raw_data_realtime=lambda:{})
        p.poll(s,lambda *args:None,42.)
        self.assertEqual(p.sample_count,0)
        s.DATA['RAW']['EEG']=np.zeros((2,6))
        with self.assertRaisesRegex(ValueError,'shape'):p.poll(s,lambda *args:None,42.)

if __name__=='__main__':unittest.main()
