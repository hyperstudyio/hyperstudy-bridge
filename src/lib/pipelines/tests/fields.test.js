import { describe, it, expect } from 'vitest';
import { list, numbers, band, bands, pairs, mix } from '../fields.js';

describe('setting fields', () => {
  it('reads back what it writes', () => {
    const cases = [
      [list, ['forehead_left', 'forehead_right']],
      [numbers, [6, 10.5]],
      [band, [20, 58]],
      [
        bands,
        [
          [4, 8],
          [8, 13],
        ],
      ],
      [
        pairs,
        [
          ['power:4-8', 'power:8-13'],
          ['a', 'b'],
        ],
      ],
      [mix, [{ name: 'difference', weights: { forehead_left: 1, forehead_right: -1 } }]],
    ];
    for (const [field, value] of cases)
      expect(field.parse(field.format(value)).value).toEqual(value);
  });

  it('says what is wrong instead of guessing', () => {
    expect(numbers.parse('6, ten').error).toMatch(/Numbers/);
    expect(band.parse('13-8').error).toMatch(/low-high/);
    expect(band.parse('').value).toBeNull();
    expect(bands.parse('4-8, oops').error).toMatch(/Bands/);
    expect(pairs.parse('a / b / c').error).toMatch(/Pairs/);
    expect(mix.parse('d = forehead_left').error).toMatch(/channel:weight/);
    expect(mix.parse('= forehead_left:1').error).toMatch(/name = channel/);
  });
});
