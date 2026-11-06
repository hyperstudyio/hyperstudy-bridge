"""Incremental SDK publication. Cursors count rows, never distinct timestamps."""
from copy import deepcopy
import numpy as np

# suffix, store, key, channels, nominal rate, LSL format, timestamp policy
STREAMS = [
    ('_EEG_raw','raw','EEG',7,125.,'double64','source_unix'),
    ('_PPG_raw','raw','PPG',4,25.,'double64','source_unix'),
    ('_IMU_raw','raw','IMU',4,50.,'double64','source_unix'),
    ('_HR_raw','raw','HR',2,1.,'double64','source_unix'),
    ('_SPO2_raw','raw','SPO2',2,1.,'double64','source_unix'),
    *[(f'_{key}_filtered','filtered',key,4,125.,'double64','observed_batch') for key in ('EEG','EOG','EMG')],
    ('_focus','scores','focus_score',1,0.,'double64','observed_history'),
    ('_sleep_stage','scores','sleep_stage',1,0.,'int64','observed_history'),
    ('_poas','scores','poas',1,0.,'double64','observed_history'),
    ('_POSTURE','scores','posture',1,0.,'string','observed_history'),
    ('_signal_quality','scores','sqc_scores',4,0.,'int64','observed_history'),
    *[(f'_{key}','scores',key,5,0.,'double64','observed_history') for key in ('alpha','beta','theta','gamma','delta')],
    *[(f'_{key.upper()}_firmware','firmware',key,ch,rate,fmt,'observed_batch')
      for key,ch,rate,fmt in [('eeg',7,125.,'int64'),('ppg',4,25.,'int64'),
          ('imu',4,50.,'double64'),('hr',2,1.,'int64'),('spo2',2,1.,'int64')]],
]


def measure_clock_pair(local_clock, wall_clock):
    """Bracket one wall-clock read; this does not measure device clock error."""
    before = local_clock()
    wall = wall_clock()
    after = local_clock()
    midpoint = before + (after - before) / 2
    return {'wall_time': wall, 'lsl_time': midpoint,
            'offset_seconds': midpoint - wall,
            'measurement_half_span_seconds': (after - before) / 2}


def timing_metadata(store, policy, clock_pair):
    source_semantics = {'raw': 'sdk_processed_unix_seconds_unverified',
                        'firmware': 'original_device_seconds_unverified'}
    metadata = {
        'timestamp_policy': policy,
        'lsl_timestamp_semantics': ('sdk_source_plus_fixed_host_clock_offset'
                                    if policy == 'source_unix' else 'bridge_poll_observation'),
        'source_timestamp_semantics': source_semantics.get(store, 'none'),
        'acquisition_alignment': 'unverified',
        'clock_discontinuity_policy': 'preserve_and_report',
        'clock_mapping_method': 'single_bracketed_host_wall_lsl_pair_fixed_for_session',
        'clock_mapping_wall_time': repr(clock_pair['wall_time']),
        'clock_mapping_lsl_time': repr(clock_pair['lsl_time']),
        'clock_mapping_measurement_half_span_seconds': repr(clock_pair['measurement_half_span_seconds']),
        'clock_mapping_accuracy_scope': 'host_read_bracket_only_not_device_accuracy',
    }
    if policy == 'source_unix':
        metadata['source_unix_to_lsl_offset'] = repr(clock_pair['offset_seconds'])
    return metadata


class SourceTiming:
    """Bounded continuity evidence, never timestamp correction or deduplication."""
    def __init__(self, rate, processed):
        self.interval = 1 / rate if processed else None
        # Tolerate float64 Unix epoch subtraction error; not device accuracy.
        self.tolerance = max(1e-6, self.interval * .01) if processed else None
        self.previous = None
        self.status = {'sample_count': 0, 'backward_count': 0, 'repeated_count': 0,
                       'nonfinite_count': 0, 'cadence_deviation_count': 0,
                       'last_source_timestamp': None, 'last_anomaly': None,
                       'expected_interval_seconds': self.interval,
                       'cadence_tolerance_seconds': self.tolerance}

    def observe(self, timestamps, observed_time):
        values = np.asarray(timestamps, dtype=np.float64)
        first_index = self.status['sample_count']
        previous = np.concatenate(([self.previous], values[:-1])) if self.previous is not None else values[:-1]
        current = values if self.previous is not None else values[1:]
        index_offset = first_index if self.previous is not None else first_index + 1
        finite = np.isfinite(values)
        valid_pairs = np.isfinite(previous) & np.isfinite(current)
        deltas = current - previous
        backwards = valid_pairs & (deltas < 0)
        repeated = valid_pairs & (deltas == 0)
        deviations = (valid_pairs & (np.abs(deltas - self.interval) > self.tolerance)
                      if self.interval is not None else np.zeros(len(deltas), dtype=bool))
        self.status['backward_count'] += int(np.count_nonzero(backwards))
        self.status['repeated_count'] += int(np.count_nonzero(repeated))
        self.status['nonfinite_count'] += int(np.count_nonzero(~finite))
        self.status['cadence_deviation_count'] += int(np.count_nonzero(deviations))
        anomalies = np.flatnonzero(backwards | deviations)
        if len(anomalies):
            i = int(anomalies[-1])
            self.status['last_anomaly'] = {
                'sample_index': index_offset + i, 'previous_source_timestamp': float(previous[i]),
                'source_timestamp': float(current[i]), 'delta_seconds': float(deltas[i]),
                'observed_lsl_time': observed_time}
        self.status['sample_count'] += len(values)
        self.previous = float(values[-1])
        self.status['last_source_timestamp'] = self.previous if finite[-1] else None


class StreamPublisher:
    def __init__(self):
        self.cursors = {}
        self.sample_count = 0
        self.timing = {}

    def timing_status(self):
        return {suffix: deepcopy(timing.status) for suffix, timing in self.timing.items()}

    def poll(self, streamer, publish, observed_time):
        stores = {'raw': streamer.DATA.get('RAW', {}),
                  'filtered': streamer.DATA.get('FILTERED', {}),
                  'scores': streamer.SCORES,
                  'firmware': streamer.get_raw_data_realtime()}
        for suffix, store, key, channels, rate, fmt, policy in STREAMS:
            value = stores[store].get('array__' + key if store == 'scores' else key)
            if value is None:
                continue
            data = np.asarray(value)
            if data.size == 0:
                if self.cursors.get(suffix, 0):
                    raise ValueError(f'{suffix} buffer shrank')
                continue
            if store == 'filtered':
                data = data.T
            elif store == 'scores' and channels == 1:
                data = data.reshape(-1, 1)
            if data.ndim != 2 or data.shape[1] != channels:
                raise ValueError(f'{suffix} unexpected shape {data.shape}')
            first = self.cursors.get(suffix, 0)
            if len(data) < first:
                raise ValueError(f'{suffix} buffer shrank')
            if len(data) == first:
                continue
            fresh = data[first:].copy()
            if fmt == 'int64':
                if not np.issubdtype(fresh.dtype, np.integer):
                    raise ValueError(f'{suffix} expected integer values')
                fresh = fresh.astype(np.int64, copy=False)
            elif fmt == 'double64':
                fresh = fresh.astype(np.float64, copy=False)
            else:
                fresh = fresh.tolist()
            # The sink either accepts this batch or raises; callers must stop on
            # errors, not retry a possibly partially published batch.
            publish(suffix, fresh, observed_time, policy)
            if store in ('raw', 'firmware'):
                if suffix not in self.timing:
                    self.timing[suffix] = SourceTiming(rate, store == 'raw')
                self.timing[suffix].observe(fresh[:, 0], observed_time)
            self.cursors[suffix] = len(data)
            self.sample_count += len(data) - first
