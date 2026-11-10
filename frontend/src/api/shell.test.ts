import { afterEach, describe, expect, it, vi } from 'vitest';
import { projectKey, route, useShell } from './shell';
import { projectHeaders, projectUrl } from './client';

afterEach(() => {
  vi.unstubAllGlobals();
  useShell.setState({ project: null, screen: 'home' });
});

describe('project routing', () => {
  it('opens Home for retired Gallery links without selecting a project', () => {
    vi.stubGlobal('location', { hash: '#/gallery' });
    expect(route()).toEqual({ project: null, screen: 'home' });
  });

  it('opens the project-independent creation screen and returns to a project', () => {
    const location = { hash: '#/' };
    vi.stubGlobal('location', location);
    useShell.getState().setScreen('new');
    expect(location.hash).toBe('#/new');
    expect(route()).toEqual({ project: null, screen: 'new' });
    useShell.getState().setProject('carina', 'sky');
    expect(route()).toEqual({ project: 'carina', screen: 'sky' });
  });

  it('keeps an export link on its original project and clears it on replacement or expiry', () => {
    vi.useFakeTimers();
    try {
      useShell.setState({ project: 'carina' });
      useShell.getState().toast('Export finished', 'orion', 'E:\\library\\sessions\\orion\\exports\\art, one.png');
      useShell.setState({ project: 'eagle' });
      expect(useShell.getState().toastFile).toEqual({ project: 'orion', filename: 'art, one.png' });
      useShell.getState().toast('Explorer unavailable');
      expect(useShell.getState().toastFile).toBeNull();
      useShell.getState().toast('Export finished', 'orion', 'E:\\exports\\another.tiff');
      vi.advanceTimersByTime(7000);
      expect(useShell.getState()).toMatchObject({ message: '', toastProject: null, toastFile: null });
    } finally { vi.useRealTimers(); }
  });
  it('keeps an early navigation until the initial project has loaded', () => {
    const location = { hash: '#/' };
    vi.stubGlobal('location', location);
    useShell.getState().setScreen('edit');
    expect(location.hash).toBe('#/');
    expect(useShell.getState().screen).toBe('edit');
    useShell.getState().setProject('orion', useShell.getState().screen);
    expect(location.hash).toBe('#/p/orion/edit');
    expect(route()).toEqual({ project: 'orion', screen: 'edit' });
  });

  it('scopes files, API headers and query caches independently of other tabs', () => {
    useShell.setState({ project: 'carina' });
    expect(projectHeaders()).toEqual({ 'X-AstroImp-Session': 'carina' });
    expect(projectUrl('/api/preview/image?size=large')).toBe('/api/preview/image?size=large&session=carina');
    expect(projectHeaders('orion')).toEqual({ 'X-AstroImp-Session': 'orion' });
    expect(projectKey('session')).toEqual(['session', 'carina']);
  });
});
