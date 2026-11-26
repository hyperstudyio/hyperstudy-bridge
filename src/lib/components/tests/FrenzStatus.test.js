import { beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/svelte';
import { listen } from '@tauri-apps/api/event';
import { getFrenzBridgeStatus } from '../../services/tauri.js';
import DeviceCard from '../DeviceCard.svelte';

vi.mock('../../services/tauri.js', () => ({
  sendTtlPulse: vi.fn(),
  listTtlDevices: vi.fn(),
  startFrenzBridge: vi.fn(),
  startFrenzNative: vi.fn(),
  getFrenzNativeStatus: vi.fn(async () => ({ state: 'stopped', streams: [], samples: {} })),
  getFrenzNativeRecent: vi.fn(async () => []),
  FRENZ_OPENFRENZ: 'openfrenz',
  checkFrenzBridgeAvailable: vi.fn(async () => true),
  getFrenzBridgeStatus: vi.fn(async () => ({
    state: 'streaming',
    streams: ['_EEG_raw'],
    sample_count: 10,
  })),
}));
vi.mock('../../services/secrets.js', () => ({ getSecret: vi.fn(async () => 'test-only') }));
vi.mock('../../stores/websocket.svelte.js', () => ({
  connectDevice: vi.fn(),
  disconnectDevice: vi.fn(),
}));
const device = {
  id: 'frenz',
  name: 'FRENZ',
  type: 'EEG',
  connection: 'LSL',
  status: 'connected',
  config: {},
};
let update;
beforeEach(() => {
  vi.clearAllMocks();
  getFrenzBridgeStatus.mockResolvedValue({
    state: 'streaming',
    streams: ['_EEG_raw'],
    sample_count: 10,
  });
  // The card also listens for the openfrenz connection's status; these tests drive the SDK helper's.
  listen.mockImplementation(async (event, callback) => {
    if (event === 'frenz_bridge_status') update = callback;
    return () => {};
  });
});
it('refreshes sidecar status after an explicit disconnect', async () => {
  vi.spyOn(window, 'alert').mockImplementation(() => {});
  render(DeviceCard, { device });
  await screen.findByText('Streaming');
  getFrenzBridgeStatus.mockResolvedValue({ state: 'stopped', streams: [], sample_count: 0 });
  await fireEvent.click(screen.getByRole('button', { name: 'Disconnect' }));
  expect(await screen.findByText('Stopped')).toBeInTheDocument();
});
it('distinguishes preserved timestamps from verified acquisition alignment', async () => {
  render(DeviceCard, { device });
  expect(await screen.findByText('Acquisition alignment unverified')).toBeInTheDocument();
});
it('reports Bluetooth EEG sequence discontinuities', async () => {
  render(DeviceCard, { device });
  await waitFor(() => expect(update).toBeTypeOf('function'));
  update({
    payload: {
      state: 'streaming',
      streams: ['_EEG_bluetooth'],
      sample_count: 18,
      timing: { _EEG_bluetooth: { sequence_discontinuity_count: 1 } },
    },
  });
  expect(await screen.findByText('Source timestamp discontinuities detected')).toBeInTheDocument();
});
it('reports processed clock jumps without treating repeated firmware seconds as errors', async () => {
  render(DeviceCard, { device });
  await waitFor(() => expect(update).toBeTypeOf('function'));
  update({
    payload: {
      state: 'streaming',
      streams: ['_EEG_raw'],
      sample_count: 10,
      timing: {
        _EEG_firmware: { repeated_count: 124, cadence_deviation_count: 0, backward_count: 0 },
      },
    },
  });
  expect(await screen.findByText('Acquisition alignment unverified')).toBeInTheDocument();
  update({
    payload: {
      state: 'streaming',
      streams: ['_EEG_raw'],
      sample_count: 10,
      timing: { _EEG_raw: { cadence_deviation_count: 2, backward_count: 1 } },
    },
  });
  expect(await screen.findByText('Source timestamp discontinuities detected')).toBeInTheDocument();
});

it('connects through openfrenz without a product key and offers the live view', async () => {
  const { startFrenzBridge, startFrenzNative, getFrenzNativeStatus } =
    await import('../../services/tauri.js');
  const { connectDevice } = await import('../../stores/websocket.svelte.js');
  const streaming = {
    state: 'streaming',
    device: 'FRENZI85',
    streams: ['FRENZI85_EEG', 'FRENZI85_EOG_openfrenz'],
    samples: { FRENZI85_EEG: 1250, FRENZI85_EOG_openfrenz: 50 },
  };
  getFrenzNativeStatus.mockResolvedValue({ state: 'stopped', streams: [], samples: {} });
  startFrenzNative.mockImplementation(async () => {
    getFrenzNativeStatus.mockResolvedValue(streaming);
    return { success: true };
  });
  render(DeviceCard, {
    device: { ...device, status: 'disconnected', config: { source: 'openfrenz' } },
  });
  expect(await screen.findByText('openfrenz (experimental)')).toBeInTheDocument();
  expect(screen.queryByText('Product Key:')).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Live signals' })).not.toBeInTheDocument();

  await fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
  await waitFor(() => expect(startFrenzNative).toHaveBeenCalledWith('test-only', [], {}));
  await waitFor(() => expect(connectDevice).toHaveBeenCalled());
  expect(startFrenzBridge).not.toHaveBeenCalled();
  expect(await screen.findByText('2 published')).toBeInTheDocument();
  expect(screen.getByText('1,300')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Live signals' })).toBeInTheDocument();
});
