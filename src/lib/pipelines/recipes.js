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
];
