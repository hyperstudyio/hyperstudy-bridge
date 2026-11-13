import { it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import FrenzDevicePicker from '../FrenzDevicePicker.svelte';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
beforeEach(() => vi.clearAllMocks());
it('requires an explicit selection from Bluetooth results', async () => {
  invoke.mockResolvedValue([{ name: 'FRENZI85', address: 'abc', rssi: -45, selectable: true }]);
  const onSelect = vi.fn();
  render(FrenzDevicePicker, { value: '', onSelect });
  await fireEvent.click(screen.getByText('Scan for FRENZ bands'));
  await fireEvent.click(await screen.findByText(/FRENZI85.*45/));
  expect(onSelect).toHaveBeenCalledWith('FRENZI85');
});
it('shows scan errors and permits another explicit scan', async () => {
  invoke.mockRejectedValue('Turn on Bluetooth');
  render(FrenzDevicePicker, { value: '', onSelect: vi.fn() });
  await fireEvent.click(screen.getByText('Scan for FRENZ bands'));
  expect(await screen.findByRole('alert')).toHaveTextContent('Turn on Bluetooth');
  expect(screen.getByText('Scan for FRENZ bands')).toBeEnabled();
});
