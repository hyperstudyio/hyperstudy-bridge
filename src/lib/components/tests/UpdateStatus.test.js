import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/svelte';
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
const toasts = () => toastStore.getToasts();
const titles = () => toasts().map(t => t.title);
const ready = () => waitFor(() => expect(handlers['update-status']).toBeDefined());

beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  for (const t of toastStore.getToasts()) toastStore.removeToast(t.id);
  for (const k of Object.keys(handlers)) delete handlers[k];
});

describe('UpdateStatus', () => {
  it('shows only the version in the footer; an up-to-date launch says nothing else', async () => {
    backend({ state: 'up_to_date', version: '0.8.28' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    expect(screen.getByText('Version: 0.8.28')).toBeInTheDocument();
    await ready();
    expect(titles()).toEqual([]);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });

  it('offers an update once, in a toast, and installs from it', async () => {
    backend({ state: 'checking' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await ready();
    emit('update-status', available);
    emit('update-status', available);
    expect(titles()).toEqual(['Update available: v0.8.29']);
    toasts()[0].actions[0].onClick(toasts()[0].id);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('install_update'));
    expect(titles()).toEqual(['Installing update']);
    emit('update-progress', { downloaded: 50, total: 200 });
    expect(toasts()[0].message).toBe('25%');
  });

  it('a check that finished before the snapshot is never overwritten by it', async () => {
    let release;
    invoke.mockImplementation(command =>
      command === 'update_status' ? new Promise(resolve => (release = resolve)) : undefined
    );
    render(UpdateStatus, { appVersion: '0.8.28' });
    await ready();
    emit('update-status', available);
    release({ state: 'up_to_date', version: '0.8.28' });
    await new Promise(r => setTimeout(r, 0));
    expect(titles()).toEqual(['Update available: v0.8.29']);
  });

  it('a check from the Help menu reports its result', async () => {
    backend({ state: 'up_to_date', version: '0.8.28' });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await ready();
    emit('menu-check-for-updates', null);
    emit('update-status', { state: 'checking' });
    emit('update-status', { state: 'failed', error: 'offline' });
    expect(titles()).toEqual(["Couldn't check for updates"]);
    emit('menu-check-for-updates', null);
    emit('update-status', { state: 'up_to_date', version: '0.8.28' });
    expect(titles()).toContain("You're up to date");
  });

  it('a refused install is explained and the update is offered again', async () => {
    backend(available, {
      install_update: () => {
        throw 'A recording is running. Install the update after it ends.';
      },
    });
    render(UpdateStatus, { appVersion: '0.8.28' });
    await waitFor(() => expect(titles()).toEqual(['Update available: v0.8.29']));
    toasts()[0].actions[0].onClick(toasts()[0].id);
    await waitFor(() => expect(titles()).toContain("Couldn't install the update"));
    expect(titles()).toContain('Update available: v0.8.29');
    expect(titles()).not.toContain('Installing update');
  });
});
