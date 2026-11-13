import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import UpdateStatus from '../UpdateStatus.svelte';
import * as toastStore from '../../stores/toast.svelte.js';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const handlers = {};
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name, handler) => {
    handlers[name] = handler;
    return () => delete handlers[name];
  }),
}));

const available = { state: 'available', version: '0.8.29', current_version: '0.8.28', notes: null };
function backend(current, extra = {}) {
  invoke.mockImplementation(async command => {
    if (command === 'update_status') return current;
    if (command in extra) return extra[command]();
    return undefined;
  });
}
const emit = (name, payload) => handlers[name]({ payload });
const toastTitles = () => toastStore.getToasts().map(t => t.title);

beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  for (const t of toastStore.getToasts()) toastStore.removeToast(t.id);
  for (const k of Object.keys(handlers)) delete handlers[k];
});

describe('UpdateStatus', () => {
  it('shows the launch check result in the footer', async () => {
    backend({ state: 'up_to_date', version: '0.8.28' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    expect(await screen.findByText('Up to date')).toBeInTheDocument();
    expect(toastTitles()).toEqual([]);
  });

  it('offers an update once, and installs from the footer', async () => {
    backend({ state: 'checking' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await waitFor(() => expect(handlers['update-status']).toBeDefined());
    emit('update-status', available);
    emit('update-status', available);
    await screen.findByText('v0.8.29 available');
    expect(toastTitles()).toEqual(['Update available: v0.8.29']);
    await userEvent.setup().click(screen.getByRole('button', { name: 'Install and restart' }));
    expect(invoke).toHaveBeenCalledWith('install_update');
  });

  it('a check that finished before the snapshot is never overwritten by it', async () => {
    let release;
    invoke.mockImplementation(command =>
      command === 'update_status' ? new Promise(resolve => (release = resolve)) : undefined
    );
    render(UpdateStatus, { appVersion: '0.8.28' });
    await waitFor(() => expect(handlers['update-status']).toBeDefined());
    emit('update-status', available);
    release({ state: 'checking' });
    await new Promise(r => setTimeout(r, 0));
    expect(screen.getByText('v0.8.29 available')).toBeInTheDocument();
    expect(screen.queryByText('Checking for updates')).not.toBeInTheDocument();
  });

  it('a failed check says so and can be retried', async () => {
    backend(
      { state: 'failed', error: 'error sending request' },
      { check_for_updates: () => ({ state: 'up_to_date', version: '0.8.28' }) }
    );
    render(UpdateStatus, { appVersion: '0.8.28' });
    expect(await screen.findByText("Couldn't check for updates")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByText('Up to date')).toBeInTheDocument();
    expect(toastTitles()).toContain("You're up to date");
  });

  it('a check from the Help menu reports its result', async () => {
    backend({ state: 'up_to_date', version: '0.8.28' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await screen.findByText('Up to date');
    emit('menu-check-for-updates', null);
    emit('update-status', { state: 'checking' });
    emit('update-status', { state: 'failed', error: 'offline' });
    await waitFor(() => expect(toastTitles()).toContain("Couldn't check for updates"));
  });

  it('a refused install is explained', async () => {
    backend(available, {
      install_update: () => {
        throw 'A recording is running. Install the update after it ends.';
      },
    });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await userEvent
      .setup()
      .click(await screen.findByRole('button', { name: 'Install and restart' }));
    await waitFor(() => expect(toastTitles()).toContain("Couldn't install the update"));
    expect(screen.getByRole('button', { name: 'Install and restart' })).toBeEnabled();
  });
});
