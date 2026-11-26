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

import { formula } from '../fields.js';

describe('mix formulas', () => {
  const known = ['forehead_left', 'forehead_right', 'comb_left', 'comb_right'];
  it('reads a weighted sum however it is written', () => {
    expect(formula.parse('forehead_left - forehead_right').value).toEqual({
      forehead_left: 1,
      forehead_right: -1,
    });
    expect(
      formula.parse('(forehead_left + forehead_right) / 2 - (comb_left + comb_right) / 2', known)
        .value
    ).toEqual({ forehead_left: 0.5, forehead_right: 0.5, comb_left: -0.5, comb_right: -0.5 });
    expect(formula.parse('2*forehead_left - 0.5 * comb_right + forehead_left').value).toEqual({
      forehead_left: 3,
      comb_right: -0.5,
    });
    expect(formula.parse('-forehead_left/4').value).toEqual({ forehead_left: -0.25 });
  });

  it('writes back what it reads', () => {
    for (const weights of [
      { forehead_left: 1, forehead_right: -1 },
      { forehead_left: 0.5, forehead_right: 0.5, comb_left: -0.5, comb_right: -0.5 },
      { comb_left: -2 },
    ])
      expect(formula.parse(formula.format(weights)).value).toEqual(weights);
    expect(formula.format({ forehead_left: 1, forehead_right: -1 })).toBe(
      'forehead_left - forehead_right'
    );
  });

  it('says what is wrong', () => {
    expect(formula.parse('forehead_left * forehead_right').error).toMatch(/not multiplied/);
    expect(formula.parse('forehead_left + 3').error).toMatch(/number on its own/);
    expect(formula.parse('comb_lft - comb_right', known).error).toMatch(
      /comb_lft is not a channel here/
    );
    expect(formula.parse('(forehead_left').error).toMatch(/bracket/);
    expect(formula.parse('forehead_left - forehead_left').error).toMatch(/cancel/);
    expect(formula.parse('forehead_left / 0').error).toMatch(/other than 0/);
  });
});
