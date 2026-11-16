import { it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.resetModules();
  invoke.mockReset();
});

it('reads a saved secret, and null when nothing is saved', async () => {
  const { getSecret } = await import('../secrets.js');
  invoke.mockResolvedValueOnce('FRENZTEST01').mockResolvedValueOnce(null);
  expect(await getSecret('frenz_device_id')).toBe('FRENZTEST01');
  expect(await getSecret('frenz_product_key')).toBeNull();
  expect(invoke).toHaveBeenNthCalledWith(1, 'get_secret', { key: 'frenz_device_id' });
  expect(invoke).toHaveBeenNthCalledWith(2, 'get_secret', { key: 'frenz_product_key' });
});

it('saves a whole form in one call and returns the keys that changed', async () => {
  const { setSecrets } = await import('../secrets.js');
  invoke.mockResolvedValue(['frenz_product_key']);
  const form = { frenz_device_id: 'FRENZTEST01', frenz_product_key: null };
  expect(await setSecrets(form)).toEqual(['frenz_product_key']);
  expect(invoke).toHaveBeenCalledExactlyOnceWith('set_secrets', { values: form });
});

it('setSecret and removeSecret save one key', async () => {
  const { setSecret, removeSecret } = await import('../secrets.js');
  invoke.mockResolvedValue([]);
  await setSecret('frenz_device_id', 'FRENZTEST01');
  await removeSecret('frenz_product_key');
  expect(invoke).toHaveBeenNthCalledWith(1, 'set_secrets', {
    values: { frenz_device_id: 'FRENZTEST01' },
  });
  expect(invoke).toHaveBeenNthCalledWith(2, 'set_secrets', {
    values: { frenz_product_key: null },
  });
});

it('runs saves in order: a later save waits for the earlier one', async () => {
  const { setSecrets } = await import('../secrets.js');
  let finishFirst;
  invoke
    .mockReturnValueOnce(new Promise(resolve => (finishFirst = resolve)))
    .mockResolvedValueOnce([]);

  const first = setSecrets({ frenz_device_id: 'FRENZTEST01' });
  const second = setSecrets({ frenz_device_id: 'FRENZTEST02' });
  await Promise.resolve();
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledTimes(1);

  finishFirst(['frenz_device_id']);
  await first;
  await second;
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(invoke).toHaveBeenLastCalledWith('set_secrets', {
    values: { frenz_device_id: 'FRENZTEST02' },
  });
});

it('a failed save reports its error and a later save still runs', async () => {
  const { setSecrets } = await import('../secrets.js');
  invoke
    .mockRejectedValueOnce('Credential store error for frenz_product_key: locked')
    .mockResolvedValueOnce(['frenz_product_key']);
  await expect(setSecrets({ frenz_product_key: 'test-only' })).rejects.toBe(
    'Credential store error for frenz_product_key: locked'
  );
  expect(await setSecrets({ frenz_product_key: 'test-only' })).toEqual(['frenz_product_key']);
  expect(invoke).toHaveBeenCalledTimes(2);
});
