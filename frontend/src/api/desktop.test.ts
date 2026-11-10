import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DOWNLOAD_EVENT, deliverExport, isDesktop, saveDownload } from './desktop';

const link = { href: '', download: '', click: vi.fn() };

// Minimal window: records listeners so a test can play the part of the desktop shell.
function fakeWindow(desktop: boolean) {
  const listeners = new Set<(event: Event) => void>();
  return {
    ...(desktop ? { __TAURI_INTERNALS__: {} } : {}),
    addEventListener: (_: string, fn: (event: Event) => void) => listeners.add(fn),
    removeEventListener: (_: string, fn: (event: Event) => void) => listeners.delete(fn),
    emit: (detail: unknown) => listeners.forEach(fn => fn({ detail } as unknown as Event)),
    listeners,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  link.href = ''; link.download = ''; link.click.mockClear();
  vi.stubGlobal('document', { createElement: vi.fn(() => link) });
  vi.stubGlobal('window', fakeWindow(false));
  vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:test');
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined);
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe('saveDownload', () => {
  it('saves a blob with an anchor click and releases it after 10 s in a browser', () => {
    expect(isDesktop()).toBe(false);
    saveDownload(new Blob(['x']), 'sky.png');
    expect(link).toMatchObject({ href: 'blob:test', download: 'sky.png' });
    expect(link.click).toHaveBeenCalledOnce();
    vi.advanceTimersByTime(9_999);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:test');
  });

  it.each(['finished', 'failed', 'canceled'])(
    'on desktop keeps the blob until the shell reports it %s, then releases only that blob', state => {
      const win = fakeWindow(true);
      vi.stubGlobal('window', win);
      expect(isDesktop()).toBe(true);
      saveDownload(new Blob(['x']), 'sky.png');
      expect(link.click).toHaveBeenCalledOnce();
      vi.advanceTimersByTime(60 * 60_000);
      expect(URL.revokeObjectURL).not.toHaveBeenCalled();
      win.emit({ url: 'blob:other', state });
      expect(URL.revokeObjectURL).not.toHaveBeenCalled();
      win.emit({ url: 'blob:test', state });
      expect(URL.revokeObjectURL).toHaveBeenCalledOnce();
      expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:test');
      expect(win.listeners.size).toBe(0);
      win.emit({ url: 'blob:test', state });
      expect(URL.revokeObjectURL).toHaveBeenCalledOnce();
    });

  it('downloads a server URL as is with no blob, timer or listener', () => {
    const win = fakeWindow(true);
    vi.stubGlobal('window', win);
    saveDownload('/api/export/full/download/m31.tiff?session=a', 'm31.tiff');
    expect(link).toMatchObject({ href: '/api/export/full/download/m31.tiff?session=a', download: 'm31.tiff' });
    expect(link.click).toHaveBeenCalledOnce();
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    expect(win.listeners.size).toBe(0);
    vi.runAllTimers();
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
  });
});

it('uses the event name the shell dispatches', () => {
  expect(DOWNLOAD_EVENT).toBe('astroimp:download');
});

it('keeps a persisted native export in its library unless Save As is requested', () => {
  vi.stubGlobal('window', fakeWindow(true));
  deliverExport('/api/export/full/download/art.png?session=a', 'art.png');
  expect(link.click).not.toHaveBeenCalled();
  expect(URL.createObjectURL).not.toHaveBeenCalled();
  deliverExport('/api/export/full/download/art.png?session=a', 'art.png', true);
  expect(link.click).toHaveBeenCalledOnce();
});

it('still downloads persisted exports in a browser', () => {
  deliverExport('/api/export/full/download/art.png?session=a', 'art.png');
  expect(link.click).toHaveBeenCalledOnce();
});
