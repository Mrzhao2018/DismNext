import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const sessionKey = 'dismnext-session-token';
const token = 'a'.repeat(64);

beforeEach(() => vi.resetModules());
afterEach(() => vi.unstubAllGlobals());

function mockSession(hash: string, remembered = '') {
  const storage = new Map(remembered ? [[sessionKey, remembered]] : []);
  const replaceState = vi.fn();
  const fetch = vi.fn();
  vi.stubGlobal('window', {
    location: { hash, pathname: '/', search: '' },
    history: { replaceState },
    sessionStorage: {
      getItem: storage.get.bind(storage),
      setItem: storage.set.bind(storage),
      removeItem: storage.delete.bind(storage),
    },
  });
  vi.stubGlobal('fetch', fetch);
  return { fetch, storage };
}

describe('desktop API session', () => {
  it('shares the launch exchange and retries after a transient failure', async () => {
    const { fetch, storage } = mockSession('#launch=one-time');
    fetch.mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(Response.json({ token }))
      .mockImplementation(() => Promise.resolve(Response.json({})));
    const { api } = await import('./api');
    await expect(api.getSysInfo()).rejects.toThrow('offline');

    await Promise.all([api.getSysInfo(), api.getSysInfo()]);
    expect(fetch.mock.calls.filter(([path]) => path === '/api/session')).toHaveLength(2);
    expect(fetch).toHaveBeenLastCalledWith('/api/sysinfo', expect.objectContaining({
      headers: expect.objectContaining({ 'X-DismNext-Token': token }),
    }));
    expect(storage.get(sessionKey)).toBe(token);
  });

  it('clears a rejected session and tells the user how to reconnect', async () => {
    const { fetch, storage } = mockSession('', token);
    fetch.mockResolvedValueOnce(Response.json({
      error: 'Missing or invalid DismNext session token',
    }, { status: 403 }));
    const { api } = await import('./api');
    await expect(api.getSysInfo()).rejects.toThrow('Reopen the window');
    expect(storage.has(sessionKey)).toBe(false);
    await expect(api.getSysInfo()).rejects.toThrow('Reopen the window');
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
