<script>
  // Live view of a band connected through openfrenz: the band's own streams and the outputs of the openfrenz
  // models, read from the backend a few times a second. Nothing here changes what is recorded or published;
  // the display options only change the drawing.
  import { getFrenzNativeRecent } from '../services/tauri.js';
  import {
    channel,
    latest,
    removeDrift,
    range,
    blinkTimes,
    blinkRate,
    contactLabel,
    defaultLayout,
    cleanLayout,
    move,
    remove,
    add,
    toggle,
    hidden,
    place,
    dropIndex,
  } from '../devices/frenzTraces.js';

  let { isOpen = $bindable(false), device = '' } = $props();

  // One hue per series, in a fixed order, checked for colour-blind separation on this app's dark surface.
  const HUES = ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181'];
  // Remembered on this computer only; the view works the same without it.
  const LAYOUT_KEY = 'hyperstudy-bridge:frenz-live-layout';
  const POLL_MS = 200;

  let seconds = $state(10);
  let flatten = $state(true);
  let traces = $state([]);
  let canvases = $state({});
  let timer = null;

  const CHART_INFO = {
    eeg: { title: 'EEG', note: 'raw counts, one lane per electrode' },
    power: { title: 'EEG power', note: 'log power, averaged over the four electrodes' },
    eyes: { title: 'Eyes', note: 'horizontal gaze position; ticks mark blinks' },
    brow: { title: 'Brow muscle', note: 'log activity, 20–58 Hz' },
    head: { title: 'Head', note: 'degrees from gravity' },
    pulse: { title: 'Pulse', note: 'optical sensor, raw counts' },
  };

  function loadLayout() {
    try {
      const saved = window.localStorage.getItem(LAYOUT_KEY);
      return saved ? cleanLayout(JSON.parse(saved)) : defaultLayout();
    } catch {
      return defaultLayout();
    }
  }
  let layout = $state(loadLayout());
  let adding = $state('');
  const removed = $derived(hidden(layout));

  // Dragging a chart by its grip: which chart, where it would land, and each chart's element to measure.
  let sections = $state({});
  let dragging = $state(null);
  let landing = $state(null);

  function landingAt(y) {
    const middles = layout
      .filter(c => c.id !== dragging)
      .map(c => sections[c.id]?.getBoundingClientRect())
      .filter(Boolean)
      .map(r => r.top + r.height / 2);
    return dropIndex(middles, y);
  }
  function startDrag(event, id) {
    event.preventDefault();
    event.currentTarget.setPointerCapture?.(event.pointerId);
    dragging = id;
    landing = landingAt(event.clientY);
  }
  function followDrag(event) {
    if (dragging) landing = landingAt(event.clientY);
  }
  function endDrag() {
    if (dragging && landing !== null) setLayout(place(layout, dragging, landing));
    dragging = null;
    landing = null;
  }
  function gripKey(event, id) {
    const by = { ArrowUp: -1, ArrowDown: 1 }[event.key];
    if (!by) return;
    event.preventDefault();
    setLayout(move(layout, id, by));
  }
  // Where the drop line is drawn: before this chart, or after the last one.
  const others = $derived(layout.filter(c => c.id !== dragging));
  const lineBefore = $derived(dragging && landing !== null ? (others[landing]?.id ?? null) : null);
  const lineAfterLast = $derived(dragging && landing !== null && landing >= others.length);

  function setLayout(next) {
    layout = next;
    try {
      window.localStorage.setItem(LAYOUT_KEY, JSON.stringify(next));
    } catch {
      // Not remembered; the view still works.
    }
  }

  const heartRate = $derived(latest(channel(traces, 'heart', 'heart:rate_bpm')));
  const heartQuality = $derived(latest(channel(traces, 'heart', 'heart:quality')));
  const contact = $derived(latest(channel(traces, 'contact', 'state')));
  const battery = $derived(latest(channel(traces, 'battery', 'percent')));
  const upright = $derived(latest(channel(traces, 'head', 'tilt:from_upright_deg')));
  const blinks = $derived(blinkRate(traces));

  function number(value, digits = 0) {
    return value === null || !Number.isFinite(value) ? '—' : value.toFixed(digits);
  }

  /** The series of each chart: label, hue, times, values, and whether each gets its own lane. */
  function seriesOf(id) {
    const pick = (name, channelName, label, hue) => {
      const found = channel(traces, name, channelName);
      return found ? { label, hue, ...found } : null;
    };
    const lists = {
      eeg: [
        pick('eeg', 'forehead_left', 'forehead left', HUES[0]),
        pick('eeg', 'forehead_right', 'forehead right', HUES[1]),
        pick('eeg', 'comb_left', 'comb left', HUES[2]),
        pick('eeg', 'comb_right', 'comb right', HUES[3]),
      ],
      eyes: [pick('eog', 'gaze_horizontal:position', 'gaze', HUES[0])],
      brow: [pick('brow-emg', 'brow:log_sd', 'brow', HUES[0])],
      head: [
        pick('head', 'tilt:pitch_deg', 'forward', HUES[0]),
        pick('head', 'tilt:roll_deg', 'to the left', HUES[1]),
      ],
      pulse: [pick('ppg', 'tag3', 'pulse', HUES[0])],
      power: [
        pick('eeg-power', 'mean:power:1-4', 'delta 1–4 Hz', HUES[0]),
        pick('eeg-power', 'mean:power:4-8', 'theta 4–8', HUES[1]),
        pick('eeg-power', 'mean:power:8-13', 'alpha 8–13', HUES[2]),
        pick('eeg-power', 'mean:power:13-30', 'beta 13–30', HUES[3]),
        pick('eeg-power', 'mean:power:30-45', 'gamma 30–45', HUES[4]),
      ],
    };
    return lists[id].filter(Boolean);
  }

  function draw(id) {
    const canvas = canvases[id];
    if (!canvas) return;
    const scale = window.devicePixelRatio || 1;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (canvas.width !== width * scale || canvas.height !== height * scale) {
      canvas.width = width * scale;
      canvas.height = height * scale;
    }
    const g = canvas.getContext('2d');
    if (!g) return;
    g.setTransform(scale, 0, 0, scale, 0, 0);
    g.clearRect(0, 0, width, height);
    const style = window.getComputedStyle(canvas);
    const ink = style.getPropertyValue('--color-text-secondary') || '#aaa';
    const grid = style.getPropertyValue('--color-border') || 'rgba(255,255,255,0.1)';

    const series = seriesOf(id);
    const lanes = id === 'eeg';
    const now = Math.max(0, ...series.map(s => s.t[s.t.length - 1] ?? 0));
    const left = 112;
    const right = width - 52;
    const x = t => right - ((now - t) / seconds) * (right - left);

    g.font = '11px system-ui, sans-serif';
    g.textBaseline = 'middle';
    if (series.length === 0 || now === 0) {
      g.fillStyle = ink;
      g.fillText('Waiting for data…', left, height / 2);
      return;
    }

    // Lines that share a panel share its scale, so their levels can be compared; lanes are scaled one by one.
    // Each line's visible stretch, with drift removed if asked (about a second: 125 EEG samples, 25 pulse).
    const eegOrPulse = id === 'eeg' || id === 'pulse';
    const shown = series.map(s => {
      const first = s.t.findIndex(t => t >= now - seconds);
      const t = first < 0 ? [] : s.t.slice(first);
      let y = first < 0 ? [] : s.y.slice(first);
      if (flatten && eegOrPulse) y = removeDrift(y, id === 'eeg' ? 125 : 25);
      return { t, y };
    });
    // Lines that share a panel share its scale, so their levels can be compared; lanes are scaled one by one.
    const shared = lanes ? null : range(shown.flatMap(v => v.y));
    const ends = [];
    series.forEach((s, k) => {
      const top = lanes ? (k * height) / series.length : 0;
      const tall = lanes ? height / series.length : height;
      const { t, y } = shown[k];
      const [low, high] = shared ?? range(y);
      const toY = v => top + tall - 4 - ((v - low) / (high - low)) * (tall - 8);

      g.strokeStyle = grid;
      g.lineWidth = 1;
      g.beginPath();
      g.moveTo(left, top + tall - 0.5);
      g.lineTo(right, top + tall - 0.5);
      g.stroke();

      g.strokeStyle = s.hue;
      g.lineWidth = 1.5;
      g.lineJoin = 'round';
      g.beginPath();
      let drawing = false;
      for (let i = 0; i < y.length; i++) {
        if (!Number.isFinite(y[i])) {
          drawing = false;
          continue;
        }
        const px = x(t[i]);
        const py = Math.min(top + tall - 2, Math.max(top + 2, toY(y[i])));
        if (drawing) g.lineTo(px, py);
        else g.moveTo(px, py);
        drawing = true;
      }
      g.stroke();

      // Each series is named beside its line, with a swatch; text stays in the text colour.
      const labelY = lanes ? top + tall / 2 : 12 + k * 15;
      g.fillStyle = s.hue;
      g.fillRect(8, labelY - 4, 8, 8);
      g.fillStyle = ink;
      g.fillText(s.label, 22, labelY);
      const last = latest({ y });
      if (last !== null && !eegOrPulse) {
        ends.push({ text: number(last, Math.abs(last) < 10 ? 1 : 0), y: toY(last) });
      }
    });
    // The latest value beside the end of each line, pushed apart where lines end close together.
    ends.sort((a, b) => a.y - b.y);
    let below = -Infinity;
    for (const end of ends) {
      end.y = Math.max(end.y, below + 12);
      below = end.y;
    }
    const overflow = Math.max(0, (ends.at(-1)?.y ?? 0) - (height - 8));
    for (const end of ends) {
      g.fillText(end.text, right + 6, Math.max(8, end.y - overflow));
    }

    if (id === 'eyes') {
      g.strokeStyle = HUES[1];
      g.lineWidth = 2;
      for (const t of blinkTimes(traces)) {
        if (t < now - seconds) continue;
        g.beginPath();
        g.moveTo(x(t), height - 14);
        g.lineTo(x(t), height - 2);
        g.stroke();
      }
      g.fillStyle = HUES[1];
      g.fillRect(8, 12 + 15 - 4, 8, 8);
      g.fillStyle = ink;
      g.fillText('blink', 22, 12 + 15);
    }

    g.fillStyle = ink;
    g.textAlign = 'right';
    g.fillText(`${seconds} s ago`, left + 52, height - 8);
    g.fillText('now', right, height - 8);
    g.textAlign = 'left';
  }

  async function refresh() {
    traces = await getFrenzNativeRecent(seconds);
    for (const chart of layout) if (!chart.collapsed) draw(chart.id);
  }

  $effect(() => {
    if (!isOpen) return;
    refresh();
    timer = setInterval(refresh, POLL_MS);
    return () => {
      clearInterval(timer);
      timer = null;
    };
  });

  function close() {
    isOpen = false;
  }
</script>

{#if isOpen}
  <div
    class="live-overlay"
    role="presentation"
    onclick={close}
    onkeydown={e => e.key === 'Escape' && close()}
  >
    <div
      class="live-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="live-title"
      tabindex="-1"
      onclick={e => e.stopPropagation()}
      onkeydown={e => e.key === 'Escape' && close()}
    >
      <header class="live-header">
        <h2 id="live-title">Live signals{device ? ` · ${device}` : ''}</h2>
        <div class="live-options">
          <label>
            Show the last
            <select bind:value={seconds}>
              <option value={5}>5 s</option>
              <option value={10}>10 s</option>
              <option value={30}>30 s</option>
            </select>
          </label>
          <label title="Subtracts each trace's one-second average, for the drawing only">
            <input type="checkbox" bind:checked={flatten} /> Remove drift
          </label>
          <button class="live-close" aria-label="Close live signals" onclick={close}>×</button>
        </div>
      </header>

      <div class="live-tiles">
        <div class="tile">
          <span class="tile-label">Heart rate</span>
          <span class="tile-value">{number(heartRate)}<small> bpm</small></span>
          <span class="tile-note">
            {heartQuality === null
              ? 'needs 30 s of pulse'
              : heartQuality >= 0.8
                ? 'good signal'
                : 'weak signal, unreliable'}
          </span>
        </div>
        <div class="tile">
          <span class="tile-label">Blinks</span>
          <span class="tile-value">{number(blinks)}<small> / min</small></span>
          <span class="tile-note">over the time shown</span>
        </div>
        <div class="tile">
          <span class="tile-label">Head from upright</span>
          <span class="tile-value">{number(upright)}<small>°</small></span>
          <span class="tile-note">the band sits at an angle</span>
        </div>
        <div class="tile">
          <span class="tile-label">Contact</span>
          <span class="tile-value tile-text">{contactLabel(contact)}</span>
          <span class="tile-note">band's own reading</span>
        </div>
        <div class="tile">
          <span class="tile-label">Battery</span>
          <span class="tile-value">{number(battery)}<small> %</small></span>
          <span class="tile-note">&nbsp;</span>
        </div>
      </div>

      <div class="live-charts">
        {#each layout as chart, index (chart.id)}
          {@const info = CHART_INFO[chart.id]}
          <section
            class="chart"
            class:tall={chart.id === 'eeg' || chart.id === 'power'}
            class:dragged={dragging === chart.id}
            class:drop-before={lineBefore === chart.id}
            class:drop-after={lineAfterLast && index === layout.length - 1}
            bind:this={sections[chart.id]}
          >
            <div class="chart-head">
              <button
                class="grip"
                aria-label="Drag to move {info.title}; or use the up and down arrow keys"
                title="Drag to move"
                onpointerdown={event => startDrag(event, chart.id)}
                onpointermove={followDrag}
                onpointerup={endDrag}
                onpointercancel={() => ((dragging = null), (landing = null))}
                onkeydown={event => gripKey(event, chart.id)}
              >
                <span aria-hidden="true">⠿</span>
              </button>
              <button
                class="chart-toggle"
                aria-expanded={!chart.collapsed}
                aria-label="{chart.collapsed ? 'Expand' : 'Collapse'} {info.title}"
                onclick={() => setLayout(toggle(layout, chart.id))}
              >
                <span class="chevron" class:closed={chart.collapsed} aria-hidden="true">▾</span>
                <h3>{info.title} <span>{info.note}</span></h3>
              </button>
              <div class="chart-actions">
                <button
                  aria-label="Remove {info.title}"
                  onclick={() => setLayout(remove(layout, chart.id))}>×</button
                >
              </div>
            </div>
            {#if !chart.collapsed}
              <canvas bind:this={canvases[chart.id]} aria-label="{info.title}: {info.note}"
              ></canvas>
            {/if}
          </section>
        {/each}
        <div class="chart-add">
          {#if removed.length}
            <select
              aria-label="Add a chart"
              bind:value={adding}
              onchange={() => {
                if (adding) setLayout(add(layout, adding));
                adding = '';
              }}
            >
              <option value="">Add a chart…</option>
              {#each removed as id (id)}
                <option value={id}>{CHART_INFO[id].title}</option>
              {/each}
            </select>
          {/if}
          <button class="reset" onclick={() => setLayout(defaultLayout())}>Reset layout</button>
        </div>
      </div>
      <p class="live-foot">
        Experimental signals from openfrenz. They were checked on one person on two days; see the
        openfrenz documentation for what each can and cannot say.
      </p>
    </div>
  </div>
{/if}

<style>
  .live-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(4px);
  }
  .live-dialog {
    display: flex;
    flex-direction: column;
    width: 94vw;
    max-width: 1100px;
    max-height: 92vh;
    overflow: hidden;
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: 12px;
    font-variant-numeric: tabular-nums;
  }
  .live-header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.9rem 1.25rem;
    border-bottom: 1px solid var(--color-border);
  }
  .live-header h2 {
    margin: 0;
    font-size: 1.1rem;
  }
  .live-options {
    display: flex;
    align-items: center;
    gap: 1rem;
    color: var(--color-text-secondary);
    font-size: 0.85rem;
  }
  .live-options select {
    margin-left: 0.35rem;
    padding: 0.2rem 0.4rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
  }
  .live-close {
    border: none;
    background: none;
    color: var(--color-text-secondary);
    font-size: 1.5rem;
    line-height: 1;
    cursor: pointer;
  }
  .live-tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 0.75rem;
    padding: 1rem 1.25rem 0.5rem;
  }
  .tile {
    display: grid;
    gap: 0.15rem;
    padding: 0.7rem 0.85rem;
    border: 1px solid var(--color-border);
    border-radius: 10px;
    background: var(--color-surface-elevated);
  }
  .tile-label,
  .tile-note {
    color: var(--color-text-secondary);
    font-size: 0.75rem;
  }
  .tile-value {
    color: var(--color-text-primary);
    font-size: 1.6rem;
    font-weight: 600;
  }
  .tile-value.tile-text {
    font-size: 1.15rem;
    line-height: 1.95rem;
  }
  .tile-value small {
    color: var(--color-text-secondary);
    font-size: 0.8rem;
    font-weight: 400;
  }
  .live-charts {
    display: grid;
    gap: 0.6rem;
    padding: 0.5rem 1.25rem;
    overflow-y: auto;
  }
  .chart h3 {
    margin: 0.2rem 0;
    font-size: 0.85rem;
  }
  .chart-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .grip {
    padding: 0 0.2rem;
    border: none;
    background: none;
    color: var(--color-text-secondary);
    font-size: 1rem;
    line-height: 1;
    cursor: grab;
    touch-action: none;
  }
  .grip:active {
    cursor: grabbing;
  }
  .chart {
    border-top: 2px solid transparent;
    border-bottom: 2px solid transparent;
  }
  .chart.dragged {
    opacity: 0.45;
  }
  .chart.drop-before {
    border-top-color: var(--color-primary);
  }
  .chart.drop-after {
    border-bottom-color: var(--color-primary);
  }
  .chart-toggle {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0;
    border: none;
    background: none;
    color: var(--color-text-primary);
    text-align: left;
    cursor: pointer;
  }
  .chevron {
    display: inline-block;
    color: var(--color-text-secondary);
    transition: transform 0.15s;
  }
  .chevron.closed {
    transform: rotate(-90deg);
  }
  .chart-actions {
    display: flex;
    margin-left: auto;
    gap: 0.25rem;
  }
  .chart-actions button,
  .chart-add button,
  .chart-add select {
    min-width: 1.8rem;
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    cursor: pointer;
  }
  .chart-actions button:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .chart-add {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
    padding-top: 0.25rem;
  }
  .chart h3 span {
    margin-left: 0.4rem;
    color: var(--color-text-secondary);
    font-weight: 400;
  }
  .chart canvas {
    display: block;
    width: 100%;
    height: 72px;
  }
  .chart.tall canvas {
    height: 200px;
  }
  .live-foot {
    margin: 0;
    padding: 0.6rem 1.25rem 0.9rem;
    color: var(--color-text-secondary);
    font-size: 0.75rem;
  }
</style>
