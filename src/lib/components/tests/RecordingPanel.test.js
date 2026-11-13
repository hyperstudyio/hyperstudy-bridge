import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, cleanup, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import RecordingPanel from '../RecordingPanel.svelte';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/path', () => ({
  documentDir: vi.fn(async () => '/Users/me/Documents'),
  join: vi.fn(async (...parts) => parts.join('/')),
}));

const idle = { state: 'idle', committed_samples: 0, path: null, error: null, streams: 0 };
const stream = {
  uid: 'u1',
  name: 'FRENZI85_EEG_raw',
  channels: 7,
  format: 'float64',
  nominal_rate: 125,
};
function backend({ status = idle, studies = [], streams = [stream] } = {}) {
  invoke.mockImplementation(async command => {
    if (command === 'list_study_recordings') return studies;
    if (command === 'discover_recording_streams') return streams;
    if (command === 'lsl_recording_status') return status;
    if (command === 'start_lsl_recording') return { ...idle, state: 'recording' };
    return undefined;
  });
}
const study = (id, created_at, extra = {}) => ({
  recording_id: id,
  created_at,
  state: 'completed',
  committed_samples: 61922,
  streams: 25,
  context: {
    experiment_id: '5o1Zr280FZW1Nd7TPiTN',
    room_id: 'room_x_8244n1',
    participant_id: 'fFFtwEg3',
  },
  upload_status: 'complete',
  upload_error: null,
  path: `/rec/${id}.sqlite`,
  ...extra,
});

beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
  backend();
});

describe('RecordingPanel', () => {
  it('lists study recordings newest first with plain upload states', async () => {
    backend({
      studies: [
        study('old', '2026-11-07T18:56:24Z'),
        study('new', '2026-11-12T04:59:10Z', {
          upload_status: 'pending',
          upload_error: 'Fresh authorization required',
        }),
      ],
    });
    render(RecordingPanel, { isOpen: true });
    const rows = await screen.findAllByRole('listitem');
    expect(within(rows[0]).getByText('Waiting to upload')).toBeInTheDocument();
    expect(within(rows[0]).getByText('Fresh authorization required')).toBeInTheDocument();
    expect(within(rows[1]).getByText('Uploaded')).toBeInTheDocument();
    expect(within(rows[1]).getByText('61,922 samples')).toBeInTheDocument();
    expect(within(rows[1]).getByText('Experiment 5o1Zr280')).toBeInTheDocument();
  });

  it('reveals a study recording by id, never by path', async () => {
    backend({ studies: [study('r1', '2026-11-12T04:59:10Z')] });
    render(RecordingPanel, { isOpen: true });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Show in Finder' }));
    expect(invoke).toHaveBeenCalledWith('reveal_recording', { recordingId: 'r1' });
  });

  it('opening manual recording finds streams and starts in the default folder', async () => {
    render(RecordingPanel, { isOpen: true });
    const user = userEvent.setup();
    await user.click(screen.getByText('Record streams yourself'));
    await user.click(await screen.findByRole('checkbox', { name: /FRENZI85_EEG_raw/ }));
    await waitFor(() =>
      expect(screen.getByLabelText('Recording folder')).toHaveValue(
        '/Users/me/Documents/HyperStudy Bridge Recordings'
      )
    );
    await user.click(screen.getByRole('button', { name: 'Start recording 1 stream' }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('start_lsl_recording', {
        request: {
          stream_uids: ['u1'],
          directory: '/Users/me/Documents/HyperStudy Bridge Recordings',
          context: { label: null },
        },
      })
    );
  });

  it('starts only the selected streams in a folder the user typed', async () => {
    render(RecordingPanel, { isOpen: true });
    const user = userEvent.setup();
    await user.click(screen.getByText('Record streams yourself'));
    await user.click(await screen.findByRole('checkbox', { name: /FRENZI85_EEG_raw/ }));
    const folder = screen.getByLabelText('Recording folder');
    await waitFor(() => expect(folder).not.toHaveValue(''));
    await user.clear(folder);
    await user.type(folder, '/tmp/recordings');
    await user.click(screen.getByRole('button', { name: 'Start recording 1 stream' }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('start_lsl_recording', {
        request: { stream_uids: ['u1'], directory: '/tmp/recordings', context: { label: null } },
      })
    );
  });

  it('shows a running recording at the top and closing does not stop it', async () => {
    backend({
      status: {
        ...idle,
        state: 'recording',
        committed_samples: 42,
        streams: 3,
        path: '/tmp/a.sqlite',
      },
    });
    const view = render(RecordingPanel, { isOpen: true });
    expect(await screen.findByText('42 samples saved from 3 streams')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop and save' })).toBeEnabled();
    view.unmount();
    expect(invoke).not.toHaveBeenCalledWith('stop_lsl_recording');
  });

  it('shows recorder failure and preserves the last committed count', async () => {
    backend({
      status: {
        ...idle,
        state: 'failed',
        committed_samples: 120,
        path: '/tmp/a.sqlite',
        error: 'Disk full',
      },
    });
    render(RecordingPanel, { isOpen: true });
    expect(await screen.findByRole('alert')).toHaveTextContent('Disk full');
    expect(screen.getByText('120 samples', { exact: false })).toBeInTheDocument();
  });

  it('distinguishes a finalized file from an unverified source tail', async () => {
    backend({
      status: {
        ...idle,
        state: 'completed',
        committed_samples: 120,
        path: '/tmp/a.sqlite',
        end_report: {
          acquisition_coverage: 'unverified',
          streams: [{ stream_uid: 'raw-1', outcome: 'timed_out' }],
        },
      },
    });
    render(RecordingPanel, { isOpen: true });
    expect(await screen.findByText(/Acquisition coverage is unverified/)).toBeInTheDocument();
    expect(screen.getByText(/raw-1: timed_out/)).toBeInTheDocument();
  });
});
