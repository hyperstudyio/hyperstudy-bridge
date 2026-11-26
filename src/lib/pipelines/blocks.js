// The openfrenz blocks the pipeline editor offers, with the settings each takes. Mirrors openfrenz's
// docs/MODELS.md "Blocks" table; blocks that need stored numbers (asr, network, normalize with weights) are left
// out, since a pipeline made here has no model.safetensors.

/** The band's streams a pipeline can read, with their channels and sample rates. */
export const STREAMS = {
  eeg: {
    label: 'EEG',
    rate: 125,
    channels: ['forehead_right', 'comb_left', 'forehead_left', 'comb_right'],
  },
  accel: { label: 'Accelerometer', rate: 50, channels: ['x', 'y', 'z'] },
  gyro: { label: 'Gyroscope', rate: 50, channels: ['x', 'y', 'z'] },
  ppg: { label: 'Pulse (PPG)', rate: 25, channels: ['tag2', 'tag3'] },
};

/** Filters a filter step can be. */
export const FILTERS = {
  bandpass: { label: 'Band-pass', fields: { low: 1, high: 45, order: 2 } },
  lowpass: { label: 'Low-pass', fields: { high: 8, order: 2 } },
  highpass: { label: 'High-pass', fields: { low: 0.5, order: 2 } },
  notch: { label: 'Notch', fields: { hz: 60, q: 30 } },
  dc_block: { label: 'Remove offset', fields: { pole: 0.99 } },
};

const bands = (key, label, value) => ({ key, label, type: 'bands', default: value });
const number = (key, label, value, help) => ({ key, label, type: 'number', default: value, help });
const flag = (key, label, value = false) => ({ key, label, type: 'bool', default: value });
const choice = (key, label, options, value) => ({
  key,
  label,
  type: 'select',
  options,
  default: value,
});
const list = (key, label, value, help) => ({ key, label, type: 'list', default: value, help });

/**
 * Each block: what it reads and gives (`signal`, `features`), whether it takes several inputs, its settings, and a
 * one-line description. Settings with `type`: number, bool, select, list (comma-separated names), bands
 * ([low, high] pairs), filters (a chain), mix (named weighted sums), pairs ([name, name] pairs).
 */
export const BLOCKS = {
  select: {
    group: 'Signal',
    label: 'Choose channels',
    reads: 'signal',
    gives: 'signal',
    about: 'Keeps some channels, in the order given.',
    settings: [list('channels', 'Channels', ['forehead_left', 'forehead_right'])],
  },
  mix: {
    group: 'Signal',
    label: 'Mix channels',
    reads: 'signal',
    gives: 'signal',
    about: 'New channels, each a weighted sum, e.g. left minus right forehead.',
    settings: [
      {
        key: 'channels',
        label: 'New channels',
        type: 'mix',
        default: [{ name: 'difference', weights: { forehead_left: 1, forehead_right: -1 } }],
      },
    ],
  },
  filter: {
    group: 'Signal',
    label: 'Filter',
    reads: 'signal',
    gives: 'signal',
    about: 'A chain of filters, carried across steps.',
    settings: [
      {
        key: 'steps',
        label: 'Filters',
        type: 'filters',
        default: [{ type: 'bandpass', low: 1, high: 45 }],
      },
      choice('start', 'Start', ['zero', 'settled'], 'settled'),
      flag('zero_phase', 'Both ways within each step (no delay, no state)'),
    ],
  },
  band_power: {
    group: 'Features',
    label: 'Band power',
    reads: 'signal',
    gives: 'features',
    about: 'Power (or magnitude) in each band of each channel.',
    settings: [
      bands('bands', 'Bands (Hz)', [
        [4, 8],
        [8, 13],
      ]),
      { key: 'relative_to', label: 'Share of (Hz, optional)', type: 'band', default: null },
      flag('log', 'Natural logarithm'),
      choice('measure', 'Measure', ['power', 'magnitude'], 'power'),
      choice('integrate', 'Integrate', ['sum', 'simpson'], 'sum'),
      choice('window', 'Window', ['hann', 'none'], 'hann'),
    ],
  },
  envelope: {
    group: 'Features',
    label: 'Envelope (Hilbert)',
    reads: 'signal',
    gives: 'features',
    about:
      'Band-passes each channel, takes its amplitude envelope with the Hilbert transform, and gives its level and spread.',
    settings: [
      { key: 'band', label: 'Band (Hz)', type: 'band', default: [20, 58] },
      list('stats', 'Statistics', ['mean', 'sd'], 'mean, sd'),
    ],
  },
  summary: {
    group: 'Features',
    label: 'Summary',
    reads: 'signal',
    gives: 'features',
    about: 'Level and spread of each channel over the step.',
    settings: [
      list('stats', 'Statistics', ['mean', 'sd'], 'mean, sd, log_sd, range, largest_step'),
    ],
  },
  quality: {
    group: 'Features',
    label: 'Signal quality',
    reads: 'signal',
    gives: 'features',
    about: 'Share of samples in each channel that repeat, clip at the limit, or are invalid.',
    settings: [
      number(
        'limit',
        'Largest value the sensor reports',
        null,
        'Leave empty if unknown: then nothing counts as clipped.'
      ),
    ],
  },
  wavelet: {
    group: 'Features',
    label: 'Wavelet (Morlet)',
    reads: 'signal',
    gives: 'features',
    about: 'Mean amplitude of each channel at each frequency, from Morlet wavelets.',
    settings: [
      { key: 'frequencies', label: 'Frequencies (Hz)', type: 'numbers', default: [6, 10] },
      number('cycles', 'Cycles', 6),
      number('trim_s', 'Leave out at each end (s)', 0),
      flag('log', 'Natural logarithm'),
    ],
  },
  coherence: {
    group: 'Features',
    label: 'Coherence',
    reads: 'signal',
    gives: 'features',
    about: 'How closely pairs of channels move together in each band.',
    settings: [
      {
        key: 'pairs',
        label: 'Channel pairs',
        type: 'pairs',
        default: [['forehead_left', 'forehead_right']],
      },
      bands('bands', 'Bands (Hz)', [[8, 13]]),
      number('average_steps', 'Average over steps', 1),
    ],
  },
  coupling: {
    group: 'Features',
    label: 'Phase-amplitude coupling',
    reads: 'signal',
    gives: 'features',
    about: "How a fast rhythm's size follows a slow rhythm's phase.",
    settings: [
      { key: 'phase_band', label: 'Phase band (Hz)', type: 'band', default: [4, 8] },
      { key: 'amplitude_band', label: 'Amplitude band (Hz)', type: 'band', default: [30, 45] },
      number('average_steps', 'Average over steps', 5),
    ],
  },
  phase_amplitude: {
    group: 'Features',
    label: 'Phase and envelope',
    reads: 'signal',
    gives: 'features',
    about: "One band's unwrapped phase against another band's envelope.",
    settings: [
      { key: 'phase_band', label: 'Phase band (Hz)', type: 'band', default: [2, 4] },
      { key: 'amplitude_band', label: 'Amplitude band (Hz)', type: 'band', default: [20, 30] },
      number('order', 'Filter order', 2),
    ],
  },
  heart: {
    group: 'Features',
    label: 'Heartbeats',
    reads: 'signal',
    gives: 'features',
    about: 'Heart rate and its variability from a pulse channel.',
    settings: [
      { key: 'channel', label: 'Channel', type: 'text', default: 'tag3' },
      number('window_steps', 'Steps gathered', 6),
    ],
  },
  peaks: {
    group: 'Events',
    label: 'Peaks',
    reads: 'signal',
    gives: 'features',
    about: 'Peaks that stand out from their surroundings, with their times.',
    settings: [
      number('threshold', 'Threshold (x ordinary size)', 4),
      number('back_s', 'Look back (s)', 2),
      number('ahead_s', 'Look ahead (s)', 0.25),
      number('min_gap_s', 'Shortest gap (s)', 0.25),
    ],
  },
  jumps: {
    group: 'Events',
    label: 'Jumps',
    reads: 'signal',
    gives: 'features',
    about: 'A level followed through abrupt changes, fading back to zero.',
    settings: [
      number('threshold', 'Threshold (x ordinary change)', 6),
      number('leak_s', 'Fade half-life (s)', 5),
    ],
  },
  tilt: {
    group: 'Features',
    label: 'Head tilt',
    reads: 'signal',
    gives: 'features',
    about: 'How the head is held, from the accelerometer.',
    settings: [],
  },
  ratios: {
    group: 'Combine',
    label: 'Ratios',
    reads: 'features',
    gives: 'features',
    about: 'One measure over another, channel by channel.',
    settings: [
      {
        key: 'pairs',
        label: 'Measure pairs',
        type: 'pairs',
        default: [['power:4-8', 'power:8-13']],
      },
      flag('log', 'Natural logarithm'),
    ],
  },
  average: {
    group: 'Combine',
    label: 'Average over channels',
    reads: 'features',
    gives: 'features',
    about: 'Each measure averaged over channels.',
    settings: [
      list('channels', 'Channels (all if empty)', []),
      { key: 'prefix', label: 'Name the averages', type: 'text', default: 'mean' },
    ],
  },
  concat: {
    group: 'Combine',
    label: 'Combine features',
    reads: 'features',
    gives: 'features',
    many: true,
    about: 'Several sets of features, one after another.',
    settings: [],
  },
  normalize: {
    group: 'Combine',
    label: 'Standardise',
    reads: 'features',
    gives: 'features',
    about: 'Each feature minus its running mean, over its running spread.',
    settings: [
      number('half_life_steps', 'Half-life (steps)', 60),
      number('warmup_steps', 'Warm-up (steps)', 2),
    ],
  },
};

/** Block names by group, for the palette. */
export function palette() {
  const groups = {};
  for (const [op, block] of Object.entries(BLOCKS)) (groups[block.group] ??= []).push(op);
  return groups;
}

/** The settings a new block starts with. */
export function defaults(op) {
  const out = {};
  for (const field of BLOCKS[op]?.settings ?? []) {
    if (field.default !== null && field.default !== undefined)
      out[field.key] = globalThis.structuredClone(field.default);
  }
  return out;
}
