"""Incremental SDK publication. Cursors count rows, never distinct timestamps."""
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


class StreamPublisher:
    def __init__(self):
        self.cursors = {}
        self.sample_count = 0

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
            self.cursors[suffix] = len(data)
            self.sample_count += len(data) - first
