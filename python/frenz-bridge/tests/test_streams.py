import unittest
from types import SimpleNamespace
import numpy as np
from frenz_streams import StreamPublisher, STREAMS

class PublisherTests(unittest.TestCase):
    def test_focus_history_keeps_identical_consecutive_scores(self):
        p = StreamPublisher(); sent = []
        s = SimpleNamespace(DATA={}, SCORES={'array__focus_score': [52., 52.]},
                            get_raw_data_realtime=lambda: {})
        p.poll(s, lambda *args: sent.append(args), 42.)
        s.SCORES['array__focus_score'].append(54.)
        p.poll(s, lambda *args: sent.append(args), 43.)
        self.assertEqual([b[0] for b in sent], ['_focus', '_focus'])
        np.testing.assert_array_equal(np.concatenate([b[1] for b in sent]), [[52.], [52.], [54.]])
        self.assertEqual(p.cursors['_focus'], 3)

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


class TimingTests(unittest.TestCase):
    def test_hybrid_metadata_does_not_claim_acquisition_or_model_window_timing(self):
        from frenz_streams import timing_metadata
        pair = {'offset_seconds': 12., 'wall_time': 30., 'lsl_time': 42.,
                'measurement_half_span_seconds': .001}
        raw = timing_metadata('bluetooth', 'packet_arrival', pair)
        focus = timing_metadata('scores', 'observed_history', pair)
        comparison = timing_metadata('raw', 'source_unix', pair)
        self.assertEqual(raw['lsl_timestamp_semantics'], 'bluetooth_callback_arrival_shared_by_nine_rows')
        self.assertEqual(raw['clock_mapping_method'], 'none_arrival_already_in_lsl_clock')
        self.assertNotIn('source_unix_to_lsl_offset', raw)
        self.assertEqual(raw['acquisition_alignment'], 'unverified')
        self.assertEqual(focus['model_window_timestamp'], 'not_exposed_by_sdk')
        self.assertEqual(comparison['acquisition_source'], 'sdk_processed_comparison_not_original_packets')

    def test_raw_steps_across_batches_are_reported_without_changing_rows(self):
        # A synthetic copy of the observed +1/-1 second pattern at 125 Hz.
        p = StreamPublisher()
        sent = []
        rows = np.zeros((5, 7))
        rows[:, 0] = [1700000000., 1700000000.008, 1700000001.016,
                      1700000000.024, 1700000000.032]
        s = SimpleNamespace(DATA={'RAW': {'EEG': rows[:2]}, 'FILTERED': {}},
                            SCORES={}, get_raw_data_realtime=lambda: {})
        p.poll(s, lambda *args: sent.append(args), 42.)
        s.DATA['RAW']['EEG'] = rows
        p.poll(s, lambda *args: sent.append(args), 43.)
        np.testing.assert_array_equal(np.concatenate([batch[1] for batch in sent]), rows)
        self.assertEqual(p.sample_count, 5)
        timing = p.timing_status()['_EEG_raw']
        self.assertEqual(timing['cadence_deviation_count'], 2)
        self.assertEqual(timing['backward_count'], 1)
        self.assertEqual(timing['repeated_count'], 0)
        self.assertEqual(timing['last_anomaly']['sample_index'], 3)
        self.assertAlmostEqual(timing['last_anomaly']['delta_seconds'], -.992, places=6)

    def test_repeated_raw_times_are_anomalies_but_firmware_groups_are_expected(self):
        p = StreamPublisher()
        s = SimpleNamespace(DATA={'RAW': {'HR': np.array([[10., 60.], [12., 60.], [12., 60.]])},
                                  'FILTERED': {}}, SCORES={},
                            get_raw_data_realtime=lambda: {'hr': np.array([[10, 60], [10, 60], [11, 60]])})
        p.poll(s, lambda *args: None, 42.)
        timing = p.timing_status()
        self.assertEqual(timing['_HR_raw']['cadence_deviation_count'], 2)
        self.assertEqual(timing['_HR_raw']['repeated_count'], 1)
        self.assertEqual(timing['_HR_firmware']['cadence_deviation_count'], 0)
        self.assertEqual(timing['_HR_firmware']['repeated_count'], 1)
        self.assertIsNone(timing['_HR_firmware']['last_anomaly'])

    def test_failed_publication_does_not_record_timing_or_advance_state(self):
        p = StreamPublisher()
        s = SimpleNamespace(DATA={'RAW': {'HR': np.array([[10., 60.], [9., 60.]])},
                                  'FILTERED': {}}, SCORES={}, get_raw_data_realtime=lambda: {})
        def fail(*args):
            raise OSError('sink failed')
        with self.assertRaises(OSError):
            p.poll(s, fail, 42.)
        self.assertEqual(p.timing_status(), {})

    def test_clock_pair_uses_bracket_midpoint_and_reports_measurement_span(self):
        import frenz_streams
        ticks = iter([100., 100.004])
        pair = frenz_streams.measure_clock_pair(lambda: next(ticks), lambda: 1700000000.)
        self.assertAlmostEqual(pair['lsl_time'], 100.002)
        self.assertAlmostEqual(pair['offset_seconds'], -1699999899.998)
        self.assertAlmostEqual(pair['measurement_half_span_seconds'], .002)
        self.assertEqual(pair['wall_time'], 1700000000.)

    def test_timing_metadata_distinguishes_source_and_observation_clocks(self):
        import frenz_streams
        raw = frenz_streams.timing_metadata('raw', 'source_unix', {'offset_seconds': 12., 'wall_time': 30., 'lsl_time': 42., 'measurement_half_span_seconds': .001})
        firmware = frenz_streams.timing_metadata('firmware', 'observed_batch', {'offset_seconds': 12., 'wall_time': 30., 'lsl_time': 42., 'measurement_half_span_seconds': .001})
        filtered = frenz_streams.timing_metadata('filtered', 'observed_batch', {'offset_seconds': 12., 'wall_time': 30., 'lsl_time': 42., 'measurement_half_span_seconds': .001})
        self.assertEqual(raw['lsl_timestamp_semantics'], 'sdk_source_plus_fixed_host_clock_offset')
        self.assertEqual(firmware['lsl_timestamp_semantics'], 'bridge_poll_observation')
        self.assertEqual(filtered['source_timestamp_semantics'], 'none')
        for metadata in (raw, firmware, filtered):
            self.assertEqual(metadata['acquisition_alignment'], 'unverified')
            self.assertEqual(metadata['clock_mapping_measurement_half_span_seconds'], '0.001')
        self.assertEqual(raw['source_unix_to_lsl_offset'], '12.0')
        self.assertNotIn('source_unix_to_lsl_offset', firmware)

if __name__ == '__main__':
    unittest.main()
