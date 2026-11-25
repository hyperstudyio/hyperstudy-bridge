import { it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import DeviceConfigModal from '../DeviceConfigModal.svelte';
import { getSecret, setSecrets } from '../../services/secrets.js';
vi.mock('../../services/secrets.js', () => ({ getSecret: vi.fn(), setSecrets: vi.fn() }));
const device = { id: 'frenz', name: 'FRENZ', config: {} };
it('enables every FRENZ option by default', async () => {
  render(DeviceConfigModal, { device, isOpen: true });
  await screen.findByText('Selected band: FRENZI85');
  const options = screen.getAllByRole('checkbox');
  expect(options).toHaveLength(19);
  for (const option of options) expect(option).toBeChecked();
});

it('warns that the three SDK raw streams can repeat and skip data', async () => {
  render(DeviceConfigModal, { device, isOpen: true });
  await screen.findByText('Selected band: FRENZI85');
  const warnings = screen.getAllByText(/can repeat one second of data and skip the next/);
  expect(warnings.map(warning => warning.textContent.split(' ')[0])).toEqual([
    'The',
    'PPG_raw',
    'IMU_raw',
  ]);
});

it('preserves explicit choices and enables them all on request', async () => {
  const onSave = vi.fn();
  render(DeviceConfigModal, {
    device: {
      ...device,
      config: { streamPoas: false, streamEegRaw: false, streamFocus: false },
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
  const saved = onSave.mock.calls[0][1];
  const streams = Object.keys(saved).filter(key => key.startsWith('stream'));
  expect(streams.length).toBeGreaterThan(0);
  expect(streams.every(key => saved[key] === true)).toBe(true);
});
beforeEach(() => {
  vi.clearAllMocks();
  getSecret.mockImplementation(async k => (k === 'frenz_device_id' ? 'FRENZI85' : 'test-only'));
  setSecrets.mockResolvedValue([]);
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
    expect.not.objectContaining({ frenzProductKey: expect.anything() })
  );
});
it('shows persistence failure and keeps configuration open', async () => {
  setSecrets.mockRejectedValue('Credential store error for frenz_product_key: test failure');
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

// A key pasted with a stray space or line break was saved as typed, and the SDK rejected it at connect.
it('saves the product key without surrounding whitespace', async () => {
  const onClose = vi.fn();
  render(DeviceConfigModal, { device, isOpen: true, onSave: vi.fn(), onClose });
  await screen.findByText('Selected band: FRENZI85');
  const input = screen.getByLabelText(/Product Key/, { selector: 'input' });
  await fireEvent.input(input, { target: { value: '  pasted-test-key \t' } });
  await fireEvent.click(screen.getByText('Save Configuration'));
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(setSecrets).toHaveBeenCalledExactlyOnceWith({
    frenz_device_id: 'FRENZI85',
    frenz_product_key: 'pasted-test-key',
  });
});

it('treats a whitespace-only product key as missing', async () => {
  render(DeviceConfigModal, { device, isOpen: true, onSave: vi.fn() });
  await screen.findByText('Selected band: FRENZI85');
  const input = screen.getByLabelText(/Product Key/, { selector: 'input' });
  await fireEvent.input(input, { target: { value: '   ' } });
  await fireEvent.click(screen.getByText('Save Configuration'));
  expect(await screen.findByText('Enter your Earable product key.')).toBeInTheDocument();
  expect(setSecrets).not.toHaveBeenCalled();
});

it('asks for Hugging Face models and a token only for the openfrenz connection', async () => {
  const onSave = vi.fn();
  render(DeviceConfigModal, {
    device: {
      ...device,
      config: { source: 'openfrenz', models: 'cosanlab/frenz-focus, bad model!' },
    },
    isOpen: true,
    onSave,
  });
  await screen.findByText('Selected band: FRENZI85');
  expect(screen.getByLabelText('Hugging Face models')).toBeVisible();
  await fireEvent.click(screen.getByText('Save Configuration'));
  expect(
    await screen.findByText(/Write each model as owner\/name: bad, model!/)
  ).toBeInTheDocument();
  expect(onSave).not.toHaveBeenCalled();
  await fireEvent.input(screen.getByLabelText('Hugging Face models'), {
    target: { value: 'cosanlab/frenz-focus' },
  });
  await fireEvent.input(document.getElementById('huggingfaceToken'), {
    target: { value: ' hf_test ' },
  });
  await fireEvent.click(screen.getByText('Save Configuration'));
  await waitFor(() => expect(onSave).toHaveBeenCalled());
  expect(onSave.mock.calls[0][1].models).toBe('cosanlab/frenz-focus');
  expect(setSecrets).toHaveBeenCalledWith(
    expect.objectContaining({ huggingface_token: 'hf_test' })
  );
  // No product key is asked for, and none is erased.
  expect(setSecrets.mock.calls[0][0]).not.toHaveProperty('frenz_product_key');
});
