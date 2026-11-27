// Ready-made pipelines to start from: each a graph the editor opens, to be changed and saved under a new name.

const stream = (stream, x, y) => ({ id: stream, kind: 'stream', stream, position: { x, y } });
const block = (id, op, settings, x, y) => ({ id, kind: 'block', op, settings, position: { x, y } });
const wire = (source, target) => ({ id: `${source}->${target}`, source, target });

export const RECIPES = [
  {
    id: 'theta-alpha',
    name: 'Theta/alpha ratio',
    about:
      'Forehead pads, filtered; theta and alpha power and their ratio, averaged over the two pads.',
    graph: () => ({
      step: { stream: 'eeg', seconds: 2 },
      nodes: [
        stream('eeg', 40, 40),
        block('front', 'select', { channels: ['forehead_left', 'forehead_right'] }, 40, 170),
        block(
          'clean',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 1, high: 45 },
            ],
            start: 'settled',
          },
          40,
          300
        ),
        block(
          'power',
          'band_power',
          {
            bands: [
              [4, 8],
              [8, 13],
            ],
            relative_to: [1, 45],
          },
          40,
          430
        ),
        block('ratio', 'ratios', { pairs: [['power:4-8', 'power:8-13']] }, 40, 560),
        block('both', 'concat', {}, 40, 690),
        block('mean', 'average', { prefix: 'mean' }, 40, 820),
      ],
      edges: [
        wire('eeg', 'front'),
        wire('front', 'clean'),
        wire('clean', 'power'),
        wire('power', 'ratio'),
        wire('power', 'both'),
        wire('ratio', 'both'),
        wire('both', 'mean'),
      ],
      outputs: ['mean'],
    }),
  },
  {
    id: 'blinks',
    name: 'Blinks',
    about:
      'Forehead pads minus side combs, 0.5-8 Hz, and the peaks that stand out (as the eog model).',
    graph: () => ({
      step: { stream: 'eeg', seconds: 0.2 },
      nodes: [
        stream('eeg', 40, 40),
        block(
          'blink_signal',
          'mix',
          {
            channels: [
              {
                name: 'blink',
                weights: {
                  forehead_left: 0.5,
                  forehead_right: 0.5,
                  comb_left: -0.5,
                  comb_right: -0.5,
                },
              },
            ],
          },
          40,
          170
        ),
        block(
          'band',
          'filter',
          { steps: [{ type: 'bandpass', low: 0.5, high: 8 }], start: 'settled' },
          40,
          300
        ),
        block(
          'blinks',
          'peaks',
          { threshold: 3, back_s: 2, ahead_s: 0.25, min_gap_s: 0.25 },
          40,
          430
        ),
      ],
      edges: [wire('eeg', 'blink_signal'), wire('blink_signal', 'band'), wire('band', 'blinks')],
      outputs: ['blinks'],
    }),
  },
  {
    id: 'brow',
    name: 'Forehead muscle',
    about:
      'Left minus right forehead, 60 Hz notch and 20-58 Hz, as a log level (as the brow-emg model).',
    graph: () => ({
      step: { stream: 'eeg', seconds: 0.2 },
      nodes: [
        stream('eeg', 40, 40),
        block(
          'brow_signal',
          'mix',
          { channels: [{ name: 'brow', weights: { forehead_left: 1, forehead_right: -1 } }] },
          40,
          170
        ),
        block(
          'muscle',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 20, high: 58 },
            ],
            start: 'settled',
          },
          40,
          300
        ),
        block('level', 'summary', { stats: ['log_sd'] }, 40, 430),
      ],
      edges: [wire('eeg', 'brow_signal'), wire('brow_signal', 'muscle'), wire('muscle', 'level')],
      outputs: ['level'],
    }),
  },
  {
    id: 'heart-head',
    name: 'Heart rate and head angle',
    about:
      'Heart rate from the pulse sensor beside head tilt from the accelerometer, two streams in one pipeline.',
    graph: () => ({
      step: { stream: 'ppg', seconds: 5 },
      nodes: [
        stream('ppg', 40, 40),
        stream('accel', 300, 40),
        block('pulse', 'heart', { channel: 'tag3', window_steps: 6 }, 40, 170),
        block('posture', 'tilt', {}, 300, 170),
        block('both', 'concat', {}, 170, 300),
      ],
      edges: [
        wire('ppg', 'pulse'),
        wire('accel', 'posture'),
        wire('pulse', 'both'),
        wire('posture', 'both'),
      ],
      outputs: ['both'],
    }),
  },
  // Dry electrodes on hair often carry mains hum and slow drift in place of EEG, while the band's own contact
  // reading still says good. Run this beside anything that reads the behind-ear combs.
  {
    id: 'signal-check',
    name: 'Electrode signal check',
    about:
      'Every 2 s, per electrode: slow noise (1-4 Hz) and mains hum against everything from 1 to 45 Hz, as logarithms. On one wearer the forehead pads gave 4 to 5 for slow noise and the combs 7 on usable days, 9 or more when they carried no EEG.',
    graph: () => ({
      step: { stream: 'eeg', seconds: 2 },
      nodes: [
        stream('eeg', 170, 40),
        block(
          'steady',
          'filter',
          { steps: [{ type: 'highpass', low: 0.5 }], start: 'settled' },
          170,
          170
        ),
        block('hum', 'band_power', { bands: [[58, 62]], relative_to: [1, 45], log: true }, 40, 300),
        block('slow', 'band_power', { bands: [[1, 4]], log: true }, 300, 300),
        block('check', 'concat', {}, 170, 430),
      ],
      edges: [
        wire('eeg', 'steady'),
        wire('steady', 'hum'),
        wire('steady', 'slow'),
        wire('hum', 'check'),
        wire('slow', 'check'),
      ],
      outputs: ['check'],
    }),
  },
  // State indices from the EEG literature, made to be recorded and judged against behaviour before anyone trusts
  // them. Their rhythms come from the behind-ear combs, since the forehead pads mostly hear the eyes and brow;
  // the combs are read against the centre forehead pad, though, so blinks reach them too, inverted.
  {
    id: 'drowsiness',
    name: 'Drowsiness signs',
    about:
      'Every 10 s: behind-ear theta, alpha and (theta+alpha)/beta, and slow against quick sideways eye movement. Untested.',
    graph: () => ({
      step: { stream: 'eeg', seconds: 10 },
      nodes: [
        stream('eeg', 40, 40),
        block('combs', 'select', { channels: ['comb_left', 'comb_right'] }, 40, 170),
        block(
          'comb_clean',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 1, high: 45 },
            ],
            start: 'settled',
          },
          40,
          300
        ),
        block(
          'comb_power',
          'band_power',
          {
            bands: [
              [4, 8],
              [8, 13],
              [4, 13],
              [13, 30],
            ],
            relative_to: [1, 45],
          },
          40,
          430
        ),
        block('slowing', 'ratios', { pairs: [['power:4-13', 'power:13-30']], log: true }, 40, 560),
        block('comb_all', 'concat', {}, 40, 690),
        block('comb', 'average', { prefix: 'comb' }, 40, 820),
        block(
          'gaze_signal',
          'mix',
          {
            channels: [
              { name: 'gaze_horizontal', weights: { forehead_left: 1, forehead_right: -1 } },
            ],
          },
          300,
          170
        ),
        block(
          'gaze_slow',
          'filter',
          {
            steps: [
              { type: 'highpass', low: 0.05 },
              { type: 'lowpass', high: 8 },
            ],
            start: 'settled',
          },
          300,
          300
        ),
        block(
          'gaze_power',
          'band_power',
          {
            bands: [
              [0.1, 0.5],
              [0.5, 5],
            ],
          },
          300,
          430
        ),
        block(
          'rolling',
          'ratios',
          { pairs: [['power:0.1-0.5', 'power:0.5-5']], log: true },
          300,
          560
        ),
        block('signs', 'concat', {}, 170, 950),
      ],
      edges: [
        wire('eeg', 'combs'),
        wire('combs', 'comb_clean'),
        wire('comb_clean', 'comb_power'),
        wire('comb_power', 'slowing'),
        wire('comb_power', 'comb_all'),
        wire('slowing', 'comb_all'),
        wire('comb_all', 'comb'),
        wire('eeg', 'gaze_signal'),
        wire('gaze_signal', 'gaze_slow'),
        wire('gaze_slow', 'gaze_power'),
        wire('gaze_power', 'rolling'),
        wire('comb', 'signs'),
        wire('rolling', 'signs'),
      ],
      outputs: ['signs'],
    }),
  },
  {
    id: 'workload',
    name: 'Workload (theta/alpha)',
    about:
      'Every 4 s: forehead theta over behind-ear alpha, the frontal-theta / posterior-alpha load index on this band. Untested; forehead theta also rises with blinks.',
    graph: () => ({
      step: { stream: 'eeg', seconds: 4 },
      nodes: [
        stream('eeg', 170, 40),
        block(
          'clean',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 1, high: 45 },
            ],
            start: 'settled',
          },
          170,
          170
        ),
        block('pads', 'select', { channels: ['forehead_left', 'forehead_right'] }, 40, 300),
        block('pad_theta', 'band_power', { bands: [[4, 8]], relative_to: [1, 45] }, 40, 430),
        block('front', 'average', { prefix: 'front' }, 40, 560),
        block('combs', 'select', { channels: ['comb_left', 'comb_right'] }, 300, 300),
        block('comb_alpha', 'band_power', { bands: [[8, 13]], relative_to: [1, 45] }, 300, 430),
        block('back', 'average', { prefix: 'comb' }, 300, 560),
        block('both', 'concat', {}, 170, 690),
        block(
          'load',
          'ratios',
          { pairs: [['front:power:4-8', 'comb:power:8-13']], log: true },
          170,
          820
        ),
        block('index', 'concat', {}, 170, 950),
      ],
      edges: [
        wire('eeg', 'clean'),
        wire('clean', 'pads'),
        wire('pads', 'pad_theta'),
        wire('pad_theta', 'front'),
        wire('clean', 'combs'),
        wire('combs', 'comb_alpha'),
        wire('comb_alpha', 'back'),
        wire('front', 'both'),
        wire('back', 'both'),
        wire('both', 'load'),
        wire('both', 'index'),
        wire('load', 'index'),
      ],
      outputs: ['index'],
    }),
  },
  {
    id: 'relaxation',
    name: 'Relaxation and arousal',
    about:
      'Every 5 s: behind-ear alpha, forehead muscle, heart rate and variability, and head movement, side by side. Untested.',
    graph: () => ({
      step: { stream: 'ppg', seconds: 5 },
      nodes: [
        stream('eeg', 40, 40),
        stream('ppg', 300, 40),
        stream('gyro', 560, 40),
        block('combs', 'select', { channels: ['comb_left', 'comb_right'] }, 40, 170),
        block(
          'comb_clean',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 1, high: 45 },
            ],
            start: 'settled',
          },
          40,
          300
        ),
        block(
          'comb_alpha',
          'band_power',
          { bands: [[8, 13]], relative_to: [1, 45], log: true },
          40,
          430
        ),
        block('alpha', 'average', { prefix: 'comb' }, 40, 560),
        block(
          'brow_signal',
          'mix',
          { channels: [{ name: 'brow', weights: { forehead_left: 1, forehead_right: -1 } }] },
          170,
          170
        ),
        block(
          'muscle',
          'filter',
          {
            steps: [
              { type: 'notch', hz: 60 },
              { type: 'bandpass', low: 20, high: 58 },
            ],
            start: 'settled',
          },
          170,
          300
        ),
        block('brow', 'summary', { stats: ['log_sd'] }, 170, 430),
        block('pulse', 'heart', { channel: 'tag3', window_steps: 6 }, 300, 170),
        block('turning', 'summary', { stats: ['sd'] }, 560, 170),
        block('head', 'average', { prefix: 'head' }, 560, 300),
        block('state', 'concat', {}, 300, 690),
      ],
      edges: [
        wire('eeg', 'combs'),
        wire('combs', 'comb_clean'),
        wire('comb_clean', 'comb_alpha'),
        wire('comb_alpha', 'alpha'),
        wire('eeg', 'brow_signal'),
        wire('brow_signal', 'muscle'),
        wire('muscle', 'brow'),
        wire('ppg', 'pulse'),
        wire('gyro', 'turning'),
        wire('turning', 'head'),
        wire('alpha', 'state'),
        wire('brow', 'state'),
        wire('pulse', 'state'),
        wire('head', 'state'),
      ],
      outputs: ['state'],
    }),
  },
];
