import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, cleanup } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import RecordingPanel from '../RecordingPanel.svelte';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
beforeEach(() => {
  cleanup(); vi.clearAllMocks();
  invoke.mockImplementation(async command => {
    if (command === 'list_study_recordings') return [];
    if (command === 'discover_recording_streams') return [{ uid: 'u1', name: 'FRENZI85_EEG_raw', channels: 7, format: 'float64', nominal_rate: 125 }];
    return { state: 'idle', committed_samples: 0, path: null, error: null };
  });
});
describe('RecordingPanel', () => {
  it('shows cloud upload state while retaining the local recording path', async () => {
    invoke.mockImplementation(async command => command === 'list_study_recordings'
      ? [{ recording_id: 'r1', context: { label: 'Private study' }, state: 'completed', upload_status: 'pending', upload_error: 'Fresh authorization required', path: '/tmp/r1.sqlite' }]
      : { state: 'idle', committed_samples: 0 });
    render(RecordingPanel, { isOpen: true });
    expect(await screen.findByText('Cloud: pending')).toBeInTheDocument();
    expect(screen.getByText('Fresh authorization required')).toBeInTheDocument();
    expect(screen.getByText('/tmp/r1.sqlite')).toBeInTheDocument();
  });
  it('starts only the selected streams in the chosen directory', async () => {
    render(RecordingPanel, { isOpen: true }); const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Find streams' }));
    await user.click(await screen.findByRole('checkbox', { name: /FRENZI85_EEG_raw/ }));
    await user.type(screen.getByLabelText('Recording folder'), '/tmp/recordings');
    await user.click(screen.getByRole('button', { name: 'Start recording' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('start_lsl_recording', {
      request: { stream_uids: ['u1'], directory: '/tmp/recordings', context: { label: null } },
    }));
  });
  it('closing the panel does not stop an active recording', async () => {
    invoke.mockImplementation(async command => command === 'list_study_recordings' ? [] : ({ state: 'recording', committed_samples: 42, path: '/tmp/a.sqlite', error: null }));
    const view = render(RecordingPanel, { isOpen: true });
    await screen.findByText('42 samples saved');
    view.unmount();
    expect(invoke).not.toHaveBeenCalledWith('stop_lsl_recording');
  });
  it('shows recorder failure and preserves the last committed count', async () => {
    invoke.mockImplementation(async command => command === 'list_study_recordings' ? [] : ({ state: 'failed', committed_samples: 120, path: '/tmp/a.sqlite', error: 'Disk full' }));
    render(RecordingPanel, { isOpen: true });
    expect(await screen.findByRole('alert')).toHaveTextContent('Disk full');
    expect(screen.getByText('120 samples saved')).toBeInTheDocument();
  });
  it('distinguishes a finalized file from an unverified source tail', async () => {
    invoke.mockImplementation(async command => command === 'list_study_recordings' ? [] : ({ state: 'completed', committed_samples: 120, path: '/tmp/a.sqlite', error: null,
      end_report: { acquisition_coverage: 'unverified', streams: [
        { stream_uid: 'raw-1', outcome: 'timed_out' },
      ] },
    }));
    render(RecordingPanel, { isOpen: true });
    expect(await screen.findByText(/Acquisition coverage is unverified/)).toBeInTheDocument();
    expect(screen.getByText(/raw-1: timed_out/)).toBeInTheDocument();
  });

});
