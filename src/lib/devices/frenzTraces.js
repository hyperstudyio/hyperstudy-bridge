// Turning what the openfrenz FRENZ source reports into things to draw. No DOM here, so it can be tested.

/** One named channel of a trace as `{ t, y }`, or null when the trace or channel is missing. */
export function channel(traces, name, channelName) {
  const trace = traces.find(t => t.name === name);
  const index = trace ? trace.channels.indexOf(channelName) : -1;
  return index < 0 ? null : { t: trace.t, y: trace.values[index] };
}

/** The last value of a channel that is a number, or null. */
export function latest(series) {
  if (!series) return null;
  for (let i = series.y.length - 1; i >= 0; i--) {
    if (Number.isFinite(series.y[i])) return series.y[i];
  }
  return null;
}

/**
 * `y` minus its average over `width` samples around each point: slow drift goes, fast changes stay.
 * Raw EEG counts drift by far more than the signal is tall, so without this a trace is a slope.
 */
export function removeDrift(y, width) {
  const n = y.length;
  if (n === 0 || width < 2) return y.slice();
  const sums = new Float64Array(n + 1);
  for (let i = 0; i < n; i++) sums[i + 1] = sums[i] + (Number.isFinite(y[i]) ? y[i] : 0);
  const half = Math.floor(width / 2);
  const out = new Array(n);
  for (let i = 0; i < n; i++) {
    const from = Math.max(0, i - half);
    const to = Math.min(n, i + half + 1);
    out[i] = y[i] - (sums[to] - sums[from]) / (to - from);
  }
  return out;
}

/**
 * The range that holds nearly all of `y` (2nd to 98th percentile, widened a little), so that one spike does
 * not flatten the rest. Never zero wide.
 */
export function range(y) {
  const sorted = y.filter(Number.isFinite).sort((a, b) => a - b);
  if (sorted.length === 0) return [-1, 1];
  const at = q => sorted[Math.min(sorted.length - 1, Math.floor(q * (sorted.length - 1)))];
  let [low, high] = [at(0.02), at(0.98)];
  if (high - low < 1e-9) {
    low -= 1;
    high += 1;
  }
  const margin = 0.15 * (high - low);
  return [low - margin, high + margin];
}

/** When blinks happened (Unix seconds), from the `eog` model's steps: each step's time plus the peak's offset. */
export function blinkTimes(traces) {
  const count = channel(traces, 'eog', 'blink:peaks');
  const offset = channel(traces, 'eog', 'blink:peak_time');
  if (!count || !offset) return [];
  const times = [];
  for (let i = 0; i < count.t.length; i++) {
    if (count.y[i] > 0 && Number.isFinite(offset.y[i])) times.push(count.t[i] + offset.y[i]);
  }
  return times;
}

/** Blinks a minute over the stretch the `eog` model covers; null until it covers five seconds. */
export function blinkRate(traces) {
  const count = channel(traces, 'eog', 'blink:peaks');
  if (!count || count.t.length < 2) return null;
  const span = count.t[count.t.length - 1] - count.t[0];
  return span < 5 ? null : (60 * blinkTimes(traces).length) / span;
}

/** What the band's contact reading (0 to 3) means. */
export function contactLabel(state) {
  if (state === null || state === undefined) return '—';
  return ['Off the head', 'Poor', 'Marginal', 'Good'][Math.round(state)] ?? '—';
}

// ---- Layout of the live view: which charts are shown, in what order, and which are collapsed.

/** Charts the live view can show, in their default order. */
export const CHARTS = ['eeg', 'power', 'eyes', 'brow', 'head', 'pulse'];

/** Traces that are the band's own streams or the built-in models; anything else is a model from the configuration. */
const BUILT_IN_TRACES = new Set([
  'eeg',
  'accel',
  'gyro',
  'ppg',
  'heart_rate',
  'spo2',
  'contact',
  'battery',
  'eog',
  'brow-emg',
  'heart',
  'head',
  'eeg-power',
]);

/** The chart for a model added in the configuration. */
export const modelChart = name => `model:${name}`;

/** Charts for the models from the configuration that are running: one per trace that is not built in. */
export function modelCharts(traces) {
  return traces.filter(t => !BUILT_IN_TRACES.has(t.name)).map(t => modelChart(t.name));
}

const knownChart = id =>
  CHARTS.includes(id) || (typeof id === 'string' && /^model:[\w.-]+$/.test(id));

/** The default layout: every chart shown, none collapsed. */
export function defaultLayout() {
  return CHARTS.map(id => ({ id, collapsed: false }));
}

/** A saved layout made safe: unknown charts dropped, duplicates removed. Anything unreadable gives the default. */
export function cleanLayout(saved) {
  if (!Array.isArray(saved)) return defaultLayout();
  const seen = new Set();
  const out = [];
  for (const entry of saved) {
    if (!entry || !knownChart(entry.id) || seen.has(entry.id)) continue;
    seen.add(entry.id);
    out.push({ id: entry.id, collapsed: Boolean(entry.collapsed) });
  }
  return out;
}

/** The layout with chart `id` moved one place up (`-1`) or down (`1`). */
export function move(layout, id, by) {
  const from = layout.findIndex(c => c.id === id);
  const to = from + by;
  if (from < 0 || to < 0 || to >= layout.length) return layout;
  const out = layout.slice();
  [out[from], out[to]] = [out[to], out[from]];
  return out;
}

/** The layout without chart `id`. */
export function remove(layout, id) {
  return layout.filter(c => c.id !== id);
}

/** The layout with chart `id` added at the end, if it is not shown already. */
export function add(layout, id) {
  return !knownChart(id) || layout.some(c => c.id === id)
    ? layout
    : [...layout, { id, collapsed: false }];
}

/** The layout with chart `id` collapsed or expanded. */
export function toggle(layout, id) {
  return layout.map(c => (c.id === id ? { ...c, collapsed: !c.collapsed } : c));
}

/** Charts not in the layout, in their default order: the standard ones, then `extra` (charts of running models). */
export function hidden(layout, extra = []) {
  return [...CHARTS, ...extra].filter(id => !layout.some(c => c.id === id));
}

/** The layout with chart `id` moved to position `index` (counted in the layout without it). */
export function place(layout, id, index) {
  const chart = layout.find(c => c.id === id);
  if (!chart) return layout;
  const rest = layout.filter(c => c.id !== id);
  const at = Math.max(0, Math.min(rest.length, index));
  return [...rest.slice(0, at), chart, ...rest.slice(at)];
}

/**
 * Where a chart dragged to height `y` lands: the number of other charts whose middle is above `y`.
 * `middles` are the vertical middles of the charts other than the dragged one, top to bottom.
 */
export function dropIndex(middles, y) {
  return middles.filter(m => m < y).length;
}
