// The openfrenz blocks the pipeline editor offers, with the settings each takes. Mirrors openfrenz's
// docs/MODELS.md "Blocks" table. Blocks that need stored numbers (asr, history and network; normalize with
// weights) are not offered, since a pipeline made here has no model.safetensors; they are known (`stored`) so that
// a model opened from the band, such as a trained one, can be shown and rewired around them.

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

/**
 * How each number of a filter is shown, with openfrenz's value when it is left empty (`order` 2, `q` 30, `pole`
 * 0.99); `low`, `high` and `hz` must be given.
 */
export const FILTER_FIELDS = {
  low: { label: 'Low (Hz)' },
  high: { label: 'High (Hz)' },
  hz: { label: 'At (Hz)' },
  order: { label: 'Order', empty: 2, whole: true },
  q: { label: 'Q', empty: 30, help: 'Centre frequency over the notch width' },
  pole: {
    label: 'Pole',
    empty: 0.99,
    help: 'Nearer 1 removes only slower drift, and settles more slowly',
  },
};

const bands = (key, label, value) => ({ key, label, type: 'bands', default: value });
const number = (key, label, value, help) => ({ key, label, type: 'number', default: value, help });
const flag = (key, label, value = false) => ({ key, label, type: 'bool', default: value });
const choice = (key, label, options, value, more = {}) => ({
  key,
  label,
  type: 'select',
  options,
  default: value,
  ...more,
});
const list = (key, label, value, help, more = {}) => ({
  key,
  label,
  type: 'list',
  default: value,
  help,
  ...more,
});

// Where a setting's choices come from (`pick`): the channels of the signal a block reads, the measures of the
// features it reads (`power:4-8`, without the channel), or their channels (`forehead_left`). Names come from
// openfrenz, which names what each block gives; `options` are fixed choices.
const STATS = ['mean', 'sd', 'log_sd', 'range', 'largest_step'];

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
    settings: [
      list('channels', 'Channels', ['forehead_left', 'forehead_right'], undefined, {
        pick: 'channels',
      }),
    ],
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
      choice('start', 'Before the first sample', ['settled', 'zero'], 'settled', {
        names: { settled: 'Assume it was steady', zero: 'Assume silence' },
        // What openfrenz does when a config leaves it out; new blocks are written with `settled`.
        absent: 'zero',
        help: "What the filter assumes came before the band's first sample. Steady (the first sample, held) avoids a large swing at the start when the signal has an offset, as raw EEG counts do; silence makes the filter ring for its first seconds.",
      }),
      {
        ...flag('zero_phase', 'No delay: filter each step forwards and backwards'),
        help: "Removes the filter's delay, but each step is filtered on its own, so rhythms slower than a step are distorted at its edges.",
      },
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
      list('stats', 'Statistics', ['mean', 'sd'], undefined, { options: STATS }),
    ],
  },
  summary: {
    group: 'Features',
    label: 'Summary',
    reads: 'signal',
    gives: 'features',
    about: 'Level and spread of each channel over the step.',
    settings: [list('stats', 'Statistics', ['mean', 'sd'], undefined, { options: STATS })],
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
        pick: 'channels',
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
      { key: 'channel', label: 'Channel', type: 'text', pick: 'channels', default: 'tag3' },
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
        pick: 'measures',
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
      list('channels', 'Channels to average (all if none)', [], undefined, { pick: 'prefixes' }),
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
  asr: {
    group: 'Stored',
    stored: true,
    label: 'Artifact removal (ASR)',
    reads: 'signal',
    gives: 'signal',
    about: 'Removes large, brief disturbances, judged against a calibration stored with the model.',
    settings: [],
  },
  history: {
    group: 'Stored',
    stored: true,
    label: 'History',
    reads: 'features',
    gives: 'sequence',
    about: 'The last few steps of features, for a network to read.',
    settings: [],
  },
  network: {
    group: 'Stored',
    stored: true,
    label: 'Network',
    reads: ['features', 'sequence'],
    gives: 'features',
    about: 'A trained network, with its weights stored with the model.',
    settings: [],
  },
};

/** Whether `block` reads what `kind` is. */
export function readsKind(block, kind) {
  return Array.isArray(block.reads) ? block.reads.includes(kind) : block.reads === kind;
}

/** Block names by group, for the palette; blocks that need stored numbers are not offered. */
export function palette() {
  const groups = {};
  for (const [op, block] of Object.entries(BLOCKS))
    if (!block.stored) (groups[block.group] ??= []).push(op);
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
