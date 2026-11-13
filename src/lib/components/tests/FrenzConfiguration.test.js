import { it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import DeviceConfigModal from '../DeviceConfigModal.svelte';
import { getSecret, setSecrets } from '../../services/stronghold.js';
vi.mock('../../services/stronghold.js', () => ({ getSecret: vi.fn(), setSecrets: vi.fn() }));
const device = { id: 'frenz', name: 'FRENZ', config: {} };
it('enables every FRENZ option by default', async () => {
  render(DeviceConfigModal, { device, isOpen: true });
  await screen.findByText('Selected band: FRENZI85');
  const options = screen.getAllByRole('checkbox');
  expect(options).toHaveLength(20);
  for (const option of options) expect(option).toBeChecked();
});

it('preserves explicit choices and enables them all on request', async () => {
  const onSave = vi.fn();
  render(DeviceConfigModal, {
    device: {
      ...device,
      config: { streamPoas: false, streamEegRaw: false, enableMarkerOutlet: false },
    },
    isOpen: true,
    onSave,
  });
  await screen.findByText('Selected band: FRENZI85');
  expect(screen.getAllByRole('checkbox').filter(input => !input.checked)).toHaveLength(3);
  await fireEvent.click(screen.getByRole('button', { name: 'Enable all' }));
  for (const option of screen.getAllByRole('checkbox')) expect(option).toBeChecked();
  expect(onSave).not.toHaveBeenCalled();
  await fireEvent.click(screen.getByText('Save Configuration'));
  await waitFor(() => expect(onSave).toHaveBeenCalled());
  expect(Object.values(onSave.mock.calls[0][1]).every(value => value === true)).toBe(true);
});
beforeEach(() => {
  vi.clearAllMocks();
  getSecret.mockImplementation(async k => (k === 'frenz_device_id' ? 'FRENZI85' : 'test-only'));
  setSecrets.mockResolvedValue();
});
it('saves the selected band and key in one operation without waiting for acquisition', async () => {
  const onSave = vi.fn(),
    onClose = vi.fn();
  render(DeviceConfigModal, { device, isOpen: true, onSave, onClose });
  await screen.findByText('Selected band: FRENZI85');
  await fireEvent.click(screen.getByText('Save Configuration'));
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(setSecrets).toHaveBeenCalledExactlyOnceWith({
    frenz_device_id: 'FRENZI85',
    frenz_product_key: 'test-only',
  });
  expect(onSave).toHaveBeenCalledWith(
    'frenz',
    expect.not.objectContaining({ frenzProductKey: expect.anything() }),
    expect.any(Object)
  );
});
it('shows persistence failure and keeps configuration open', async () => {
  setSecrets.mockRejectedValue(Error('test disk failure'));
  const onClose = vi.fn();
  render(DeviceConfigModal, { device, isOpen: true, onSave: vi.fn(), onClose });
  await screen.findByText('Selected band: FRENZI85');
  await fireEvent.click(screen.getByText('Save Configuration'));
  expect(await screen.findByRole('alert')).toHaveTextContent('Could not save configuration');
  expect(onClose).not.toHaveBeenCalled();
});

it('shows and hides the current key without changing or saving it', async () => {
  const onSave = vi.fn();
  render(DeviceConfigModal, { device, isOpen: true, onSave });
  await screen.findByText('Selected band: FRENZI85');
  const input = screen.getByLabelText(/Product Key/, { selector: 'input' });
  expect(input).toHaveAttribute('type', 'password');
  await fireEvent.input(input, { target: { value: 'edited-test-key' } });
  await fireEvent.click(screen.getByRole('button', { name: 'Show Product Key' }));
  expect(input).toHaveAttribute('type', 'text');
  expect(input).toHaveValue('edited-test-key');
  await fireEvent.click(screen.getByRole('button', { name: 'Hide Product Key' }));
  expect(input).toHaveAttribute('type', 'password');
  expect(input).toHaveValue('edited-test-key');
  expect(onSave).not.toHaveBeenCalled();
  expect(setSecrets).not.toHaveBeenCalled();
});

it('hides the key again when configuration is reopened', async () => {
  const view = render(DeviceConfigModal, { device, isOpen: true });
  await screen.findByText('Selected band: FRENZI85');
  await fireEvent.click(screen.getByRole('button', { name: 'Show Product Key' }));
  await view.rerender({ isOpen: false });
  await view.rerender({ isOpen: true });
  await screen.findByText('Selected band: FRENZI85');
  expect(screen.getByLabelText(/Product Key/, { selector: 'input' })).toHaveAttribute(
    'type',
    'password'
  );
});
