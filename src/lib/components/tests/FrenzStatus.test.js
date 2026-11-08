import {beforeEach, expect, it, vi} from 'vitest';
import {render, screen, waitFor, fireEvent} from '@testing-library/svelte';
import {listen} from '@tauri-apps/api/event';
import {getFrenzBridgeStatus} from '../../services/tauri.js';
import DeviceCard from '../DeviceCard.svelte';

vi.mock('../../services/tauri.js', () => ({
  sendTtlPulse: vi.fn(), listTtlDevices: vi.fn(), startFrenzBridge: vi.fn(),
  checkFrenzBridgeAvailable: vi.fn(async () => true),
  getFrenzBridgeStatus: vi.fn(async () => ({state: 'streaming', streams: ['_EEG_raw'], sample_count: 10})),
}));
vi.mock('../../services/stronghold.js', () => ({getSecret: vi.fn(async () => 'test-only')}));
vi.mock('../../stores/websocket.svelte.js', () => ({connectDevice: vi.fn(), disconnectDevice: vi.fn()}));
const device = {id:'frenz',name:'FRENZ',type:'EEG',connection:'LSL',status:'connected',config:{}};
let update;
beforeEach(() => {
  vi.clearAllMocks();
  getFrenzBridgeStatus.mockResolvedValue({state:'streaming',streams:['_EEG_raw'],sample_count:10});
  listen.mockImplementation(async (_event, callback) => {update = callback; return () => {};});
});
it('refreshes sidecar status after an explicit disconnect', async () => {
  vi.spyOn(window, 'alert').mockImplementation(() => {});
  render(DeviceCard, {device});
  await screen.findByText('Streaming');
  getFrenzBridgeStatus.mockResolvedValue({state:'stopped',streams:[],sample_count:0});
  await fireEvent.click(screen.getByRole('button', {name:'Disconnect'}));
  expect(await screen.findByText('Stopped')).toBeInTheDocument();
});
it('distinguishes preserved timestamps from verified acquisition alignment', async () => {
  render(DeviceCard, {device});
  expect(await screen.findByText('Acquisition alignment unverified')).toBeInTheDocument();
});
it('reports Bluetooth EEG sequence discontinuities', async () => {
  render(DeviceCard, {device});
  await waitFor(() => expect(update).toBeTypeOf('function'));
  update({payload:{state:'streaming',streams:['_EEG_bluetooth'],sample_count:18,timing:{_EEG_bluetooth:{sequence_discontinuity_count:1}}}});
  expect(await screen.findByText('Source timestamp discontinuities detected')).toBeInTheDocument();
});
it('reports processed clock jumps without treating repeated firmware seconds as errors', async () => {
  render(DeviceCard, {device});
  await waitFor(() => expect(update).toBeTypeOf('function'));
  update({payload:{state:'streaming',streams:['_EEG_raw'],sample_count:10,timing:{_EEG_firmware:{repeated_count:124,cadence_deviation_count:0,backward_count:0}}}});
  expect(await screen.findByText('Acquisition alignment unverified')).toBeInTheDocument();
  update({payload:{state:'streaming',streams:['_EEG_raw'],sample_count:10,timing:{_EEG_raw:{cadence_deviation_count:2,backward_count:1}}}});
  expect(await screen.findByText('Source timestamp discontinuities detected')).toBeInTheDocument();
});
