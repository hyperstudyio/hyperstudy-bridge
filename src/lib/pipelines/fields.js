// Settings as text in the editor's fields, and back. Each `parse` returns { value } or { error }.

const num = text => {
  const v = Number(text.trim());
  return text.trim() !== '' && Number.isFinite(v) ? v : null;
};

/** Names: "a, b, c" */
export const list = {
  format: v => (v ?? []).join(', '),
  parse: text => ({
    value: text
      .split(',')
      .map(s => s.trim())
      .filter(Boolean),
  }),
};

/** Numbers: "6, 10, 20" */
export const numbers = {
  format: v => (v ?? []).join(', '),
  parse: text => {
    const parts = text
      .split(',')
      .map(s => s.trim())
      .filter(Boolean);
    const values = parts.map(num);
    return values.some(v => v === null) || !values.length
      ? { error: 'Numbers separated by commas, e.g. 6, 10' }
      : { value: values };
  },
};

/** One band: "20-58", or empty for none. */
export const band = {
  format: v => (Array.isArray(v) ? `${v[0]}-${v[1]}` : ''),
  parse: text => {
    if (!text.trim()) return { value: null };
    const [a, b] = text.split('-').map(num);
    return a !== null && b !== null && a < b && text.split('-').length === 2
      ? { value: [a, b] }
      : { error: 'A band as low-high in Hz, e.g. 8-13' };
  },
};

/** Bands: "4-8, 8-13" */
export const bands = {
  format: v => (v ?? []).map(b => `${b[0]}-${b[1]}`).join(', '),
  parse: text => {
    const parts = text
      .split(',')
      .map(s => s.trim())
      .filter(Boolean);
    const parsed = parts.map(p => band.parse(p).value);
    return !parts.length || parsed.some(b => !b)
      ? { error: 'Bands as low-high, separated by commas, e.g. 4-8, 8-13' }
      : { value: parsed };
  },
};

/** Pairs: "forehead_left / forehead_right; power:4-8 / power:8-13" */
export const pairs = {
  format: v => (v ?? []).map(([a, b]) => `${a} / ${b}`).join('; '),
  parse: text => {
    const parts = text
      .split(';')
      .map(s => s.trim())
      .filter(Boolean);
    const parsed = parts.map(p => p.split('/').map(s => s.trim()));
    return !parts.length || parsed.some(p => p.length !== 2 || !p[0] || !p[1])
      ? { error: 'Pairs as a / b, separated by semicolons' }
      : { value: parsed };
  },
};

/** Mixes: one per line, "difference = forehead_left:1, forehead_right:-1" */
export const mix = {
  format: v =>
    (v ?? [])
      .map(
        m =>
          `${m.name} = ${Object.entries(m.weights)
            .map(([c, w]) => `${c}:${w}`)
            .join(', ')}`
      )
      .join('\n'),
  parse: text => {
    const lines = text
      .split('\n')
      .map(s => s.trim())
      .filter(Boolean);
    const out = [];
    for (const line of lines) {
      const [name, rest] = line.split('=').map(s => s?.trim());
      const weights = {};
      for (const part of (rest ?? '')
        .split(',')
        .map(s => s.trim())
        .filter(Boolean)) {
        const [channel, weight] = part.split(':').map(s => s.trim());
        if (!channel || num(weight ?? '') === null)
          return { error: `"${part}" is not channel:weight` };
        weights[channel] = num(weight);
      }
      if (!name || !Object.keys(weights).length) {
        return { error: 'One channel per line: name = channel:weight, channel:weight' };
      }
      out.push({ name, weights });
    }
    return out.length ? { value: out } : { error: 'At least one channel' };
  },
};

/** The kinds of field there are, by setting type. */
export const FORMATS = { list, numbers, band, bands, pairs, mix };

/**
 * A weighted sum written as a formula, e.g. `forehead_left - forehead_right` or
 * `(forehead_left + forehead_right) / 2 - comb_left / 2`, and the `{ channel: weight }` it means. Only sums of
 * channels times numbers: a channel times a channel, or a number on its own, is refused with why. `known`, when
 * given, lists the channels that may be named.
 */
export const formula = {
  format: weights => {
    const terms = Object.entries(weights ?? {}).filter(([, w]) => w !== 0);
    return terms
      .map(([name, w], i) => {
        const size = Math.abs(w);
        const term = size === 1 ? name : `${+size.toPrecision(6)} * ${name}`;
        return i === 0 ? (w < 0 ? `-${term}` : term) : `${w < 0 ? '-' : '+'} ${term}`;
      })
      .join(' ');
  },
  parse: (text, known = null) => {
    const tokens =
      String(text).match(/\d*\.?\d+(?:e[+-]?\d+)?|[A-Za-z_][\w.]*|[-+*/()]|\S/gi) ?? [];
    let at = 0;
    const peek = () => tokens[at];
    const fail = message => {
      throw new Error(message);
    };
    // Each value is linear: a constant and weights.
    const add = (a, b, sign) => {
      const weights = { ...a.weights };
      for (const [k, w] of Object.entries(b.weights)) weights[k] = (weights[k] ?? 0) + sign * w;
      return { constant: a.constant + sign * b.constant, weights };
    };
    const scale = (a, f) => ({
      constant: a.constant * f,
      weights: Object.fromEntries(Object.entries(a.weights).map(([k, w]) => [k, w * f])),
    });
    const plain = a => Object.keys(a.weights).length === 0;
    const factor = () => {
      const token = peek();
      if (token === undefined) fail('The formula ends too soon.');
      at += 1;
      if (token === '-') return scale(factor(), -1);
      if (token === '+') return factor();
      if (token === '(') {
        const inner = sum();
        if (peek() !== ')') fail('A bracket is not closed.');
        at += 1;
        return inner;
      }
      if (/^\d*\.?\d+(?:e[+-]?\d+)?$/i.test(token)) return { constant: Number(token), weights: {} };
      if (/^[A-Za-z_]/.test(token)) {
        if (known && !known.includes(token))
          fail(`${token} is not a channel here. Channels: ${known.join(', ')}.`);
        return { constant: 0, weights: { [token]: 1 } };
      }
      return fail(`Unexpected ${token}.`);
    };
    const product = () => {
      let value = factor();
      while (peek() === '*' || peek() === '/') {
        const op = tokens[at++];
        const next = factor();
        if (op === '/') {
          if (!plain(next) || next.constant === 0) fail('Divide only by a number other than 0.');
          value = scale(value, 1 / next.constant);
        } else if (plain(next)) value = scale(value, next.constant);
        else if (plain(value)) value = scale(next, value.constant);
        else fail('Channels can be added and scaled, not multiplied together.');
      }
      return value;
    };
    const sum = () => {
      let value = product();
      while (peek() === '+' || peek() === '-') {
        const sign = tokens[at++] === '-' ? -1 : 1;
        value = add(value, product(), sign);
      }
      return value;
    };
    try {
      if (!tokens.length) fail('Write a sum of channels, e.g. forehead_left - forehead_right.');
      const value = sum();
      if (at < tokens.length) fail(`Unexpected ${tokens[at]}.`);
      if (Math.abs(value.constant) > 1e-12)
        fail('A number on its own cannot be added: a mix is a weighted sum of channels.');
      const weights = Object.fromEntries(
        Object.entries(value.weights)
          .map(([k, w]) => [k, +w.toPrecision(12)])
          .filter(([, w]) => w !== 0)
      );
      if (!Object.keys(weights).length) fail('The channels cancel out.');
      return { value: weights };
    } catch (error) {
      return { error: error.message };
    }
  },
};
