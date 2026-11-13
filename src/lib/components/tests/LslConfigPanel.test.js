import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup, within, waitFor } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import LslConfigPanel from '../LslConfigPanel.svelte';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const inlet = (name, uid, extra = {}) => ({
  name,
  uid,
  type: 'EEG',
  sample_count: 1200,
  seconds_since_last_receive: 0.1,
  ...extra,
});
function backend({ receiving = [], publishing = [], network = [] } = {}) {
  invoke.mockImplementation(async command => {
    if (command === 'lsl_bridge_streams') return { receiving, publishing };
    if (command === 'discover_recording_streams') return network;
    throw new Error(`unexpected ${command}`);
  });
}
beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('LslConfigPanel', () => {
  it('lists the streams Bridge receives with their health, without a refresh', async () => {
    backend({
      receiving: [
        inlet('FRENZI85_EEG_raw', 'a'),
        inlet('FRENZI85_PPG_raw', 'b', { seconds_since_last_receive: 9.4 }),
        inlet('Neon Gaze', 'c', { sample_count: 0 }),
        inlet('FRENZI85_sleep_stage', 'd', { seconds_since_last_receive: 25 }),
      ],
      network: [
        { uid: 'b', name: 'FRENZI85_PPG_raw', channels: 4, nominal_rate: 25 },
        { uid: 'd', name: 'FRENZI85_sleep_stage', channels: 1, nominal_rate: 0 },
      ],
    });
    render(LslConfigPanel, { isOpen: true });
    const section = (await screen.findByText('FRENZI85_EEG_raw')).closest('section');
    expect(within(section).getByText('Live')).toBeInTheDocument();
    expect(await within(section).findByText('No data for 9 s')).toBeInTheDocument();
    // Irregular streams are quiet by design: report the gap, never as a fault.
    expect(within(section).getByText('Last data 25 s ago')).toBeInTheDocument();
    expect(within(section).getByText('Waiting for data')).toBeInTheDocument();
    expect(within(section).getAllByText('1,200 samples')).toHaveLength(3);
    expect(screen.queryByRole('button', { name: /refresh/i })).not.toBeInTheDocument();
  });

  it('marks which network streams Bridge receives or publishes', async () => {
    backend({
      receiving: [inlet('FRENZI85_EEG_raw', 'a')],
      publishing: [{ name: 'HyperStudy_FRENZ_Markers', type: 'Markers', sample_count: 3 }],
      network: [
        { uid: 'a', name: 'FRENZI85_EEG_raw', channels: 7, nominal_rate: 125 },
        { uid: 'm', name: 'HyperStudy_FRENZ_Markers', channels: 1, nominal_rate: 0 },
        { uid: 'x', name: 'OtherApp_Audio', channels: 2, nominal_rate: 44100 },
      ],
    });
    render(LslConfigPanel, { isOpen: true });
    const network = (await screen.findByText('OtherApp_Audio')).closest('section');
    const rows = within(network).getAllByRole('listitem');
    expect(within(rows[0]).getByText('Bridge receives')).toBeInTheDocument();
    expect(within(rows[1]).getByText('From Bridge')).toBeInTheDocument();
    expect(within(rows[2]).queryByText(/Bridge/)).not.toBeInTheDocument();
  });

  it('says what to do when nothing is connected', async () => {
    backend();
    render(LslConfigPanel, { isOpen: true });
    expect(await screen.findByText(/Connect a FRENZ band or Neon/)).toBeInTheDocument();
    expect(await screen.findByText(/No streams found/)).toBeInTheDocument();
  });
  it('clears a failed refresh once Bridge answers again', async () => {
    let fail = true;
    invoke.mockImplementation(async command => {
      if (command === 'lsl_bridge_streams' && fail) throw new Error('Bridge busy');
      if (command === 'lsl_bridge_streams') return { receiving: [], publishing: [] };
      return [];
    });
    render(LslConfigPanel, { isOpen: true });
    expect(await screen.findByRole('alert')).toHaveTextContent('Bridge busy');
    fail = false;
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument(), {
      timeout: 2500,
    });
  });
});
