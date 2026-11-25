import { describe, it, expect } from 'vitest';
import {
  channel,
  latest,
  removeDrift,
  range,
  blinkTimes,
  blinkRate,
  contactLabel,
} from '../frenzTraces.js';

const eog = (t, peaks, offsets) => ({
  name: 'eog',
  channels: ['blink:peaks', 'blink:peak_time', 'gaze_horizontal:position'],
  t,
  values: [peaks, offsets, t.map(() => 0)],
});

describe('frenzTraces', () => {
  it('finds a channel by trace and channel name', () => {
    const traces = [
      {
        name: 'eeg',
        channels: ['a', 'b'],
        t: [1, 2],
        values: [
          [10, 11],
          [20, 21],
        ],
      },
    ];
    expect(channel(traces, 'eeg', 'b')).toEqual({ t: [1, 2], y: [20, 21] });
    expect(channel(traces, 'eeg', 'c')).toBeNull();
    expect(channel(traces, 'ppg', 'a')).toBeNull();
  });

  it('gives the last value that is a number', () => {
    expect(latest({ t: [1, 2, 3], y: [5, 7, NaN] })).toBe(7);
    expect(latest({ t: [], y: [] })).toBeNull();
    expect(latest(null)).toBeNull();
  });

  it('removes a slope and keeps a fast wiggle', () => {
    const y = Array.from({ length: 500 }, (_, i) => 1000 * i + (i % 2 ? 5 : -5));
    const flat = removeDrift(y, 51);
    const middle = flat.slice(100, 400);
    expect(Math.max(...middle.map(Math.abs))).toBeLessThan(6);
    expect(Math.min(...middle.map(Math.abs))).toBeGreaterThan(4);
    expect(removeDrift([], 51)).toEqual([]);
  });

  it('scales to the bulk of the data, not to a spike', () => {
    const y = Array.from({ length: 1000 }, (_, i) => (i % 10) - 5);
    y[500] = 1e6;
    const [low, high] = range(y);
    expect(low).toBeLessThan(-5);
    expect(high).toBeGreaterThan(4);
    expect(high).toBeLessThan(10);
    expect(range([3, 3, 3])).toEqual([1.7, 4.3]);
    expect(range([])).toEqual([-1, 1]);
  });

  it('times blinks from the step time and the offset of the peak', () => {
    const traces = [eog([10, 10.2, 10.4, 10.6], [0, 1, 0, 1], [NaN, -0.6, NaN, -0.5])];
    expect(blinkTimes(traces)).toEqual([10.2 - 0.6, 10.6 - 0.5]);
    expect(blinkTimes([])).toEqual([]);
  });

  it('gives blinks a minute once five seconds are covered', () => {
    const t = Array.from({ length: 101 }, (_, i) => i * 0.2);
    const peaks = t.map((_, i) => (i % 25 === 24 ? 1 : 0));
    const offsets = peaks.map(p => (p ? -0.5 : NaN));
    expect(blinkRate([eog(t, peaks, offsets)])).toBeCloseTo(12, 5);
    expect(blinkRate([eog([0, 0.2], [0, 1], [NaN, -0.5])])).toBeNull();
  });

  it('names the contact states', () => {
    expect(contactLabel(3)).toBe('Good');
    expect(contactLabel(0)).toBe('Off the head');
    expect(contactLabel(null)).toBe('—');
  });
});

import {
  CHARTS,
  defaultLayout,
  cleanLayout,
  move,
  remove,
  add,
  toggle,
  hidden,
  place,
  dropIndex,
} from '../frenzTraces.js';

describe('live view layout', () => {
  it('moves, removes, adds back and collapses charts', () => {
    let layout = defaultLayout();
    expect(layout.map(c => c.id)).toEqual(CHARTS);
    layout = move(layout, 'eyes', -1);
    expect(layout.map(c => c.id).slice(0, 3)).toEqual(['eeg', 'eyes', 'power']);
    expect(move(layout, 'eeg', -1)).toBe(layout);
    layout = remove(layout, 'pulse');
    expect(hidden(layout)).toEqual(['pulse']);
    layout = add(layout, 'pulse');
    expect(layout.at(-1)).toEqual({ id: 'pulse', collapsed: false });
    expect(add(layout, 'pulse')).toBe(layout);
    expect(add(layout, 'nonsense')).toBe(layout);
    layout = toggle(layout, 'brow');
    expect(layout.find(c => c.id === 'brow').collapsed).toBe(true);
  });

  it('keeps only what it knows from a saved layout', () => {
    expect(cleanLayout(null)).toEqual(defaultLayout());
    expect(
      cleanLayout([
        { id: 'head', collapsed: 1 },
        { id: 'head' },
        { id: 'focus' },
        null,
        { id: 'eeg' },
      ])
    ).toEqual([
      { id: 'head', collapsed: true },
      { id: 'eeg', collapsed: false },
    ]);
  });

  it('places a dragged chart where it is dropped', () => {
    const ids = l => l.map(c => c.id);
    const layout = defaultLayout();
    expect(ids(place(layout, 'pulse', 0))).toEqual([
      'pulse',
      'eeg',
      'power',
      'eyes',
      'brow',
      'head',
    ]);
    expect(ids(place(layout, 'eeg', 2))).toEqual(['power', 'eyes', 'eeg', 'brow', 'head', 'pulse']);
    expect(ids(place(layout, 'eeg', 99)).at(-1)).toBe('eeg');
    expect(place(layout, 'nonsense', 0)).toBe(layout);
    expect(dropIndex([100, 200, 300], 50)).toBe(0);
    expect(dropIndex([100, 200, 300], 250)).toBe(2);
    expect(dropIndex([100, 200, 300], 400)).toBe(3);
  });
});
