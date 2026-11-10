export type Theme = 'dark' | 'light' | 'system';
export interface Appearance { theme: Theme; primary: string; secondary: string }
export const appearanceKey = 'astroimp-appearance';
export const defaults: Appearance = { theme: 'dark', primary: '#e9b35a', secondary: '#5fc9d6' };

export function normalizeColor(value: unknown): string | null {
  return typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value) ? value.toLowerCase() : null;
}
export function luminance(hex: string): number {
  const channels = [1, 3, 5].map(i => {
    const value = parseInt(hex.slice(i, i + 2), 16) / 255;
    return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
  });
  return channels[0] * .2126 + channels[1] * .7152 + channels[2] * .0722;
}
export function accentText(hex: string): string {
  const light = (1.05) / (luminance(hex) + .05);
  const dark = (luminance(hex) + .05) / .05;
  return dark >= light ? '#000000' : '#ffffff';
}
export function readableAccent(hex: string, surface: string, mix: '#ffffff' | '#000000'): string {
  const background = luminance(surface);
  for (let percent = 100; percent >= 0; percent--) {
    const color = '#' + [1, 3, 5].map(i => Math.round(
      parseInt(hex.slice(i, i + 2), 16) * percent / 100
      + parseInt(mix.slice(i, i + 2), 16) * (1 - percent / 100),
    ).toString(16).padStart(2, '0')).join('');
    const foreground = luminance(color);
    if ((Math.max(background, foreground) + .05) / (Math.min(background, foreground) + .05) >= 4.5)
      return `color-mix(in srgb, ${hex} ${percent}%, ${mix})`;
  }
  return mix;
}
export function parseAppearance(value: unknown): Appearance {
  const saved = value && typeof value === 'object' ? value as Partial<Appearance> : {};
  return {
    theme: ['dark', 'light', 'system'].includes(saved.theme ?? '') ? saved.theme! : defaults.theme,
    primary: normalizeColor(saved.primary) ?? defaults.primary,
    secondary: normalizeColor(saved.secondary) ?? defaults.secondary,
  };
}
export function readAppearance(storage?: Pick<Storage, 'getItem'>): Appearance {
  try { return parseAppearance(JSON.parse((storage ?? localStorage).getItem(appearanceKey) ?? 'null')); }
  catch { return { ...defaults }; }
}
export function saveAppearance(value: Appearance, storage?: Pick<Storage, 'setItem'>): void {
  try { (storage ?? localStorage).setItem(appearanceKey, JSON.stringify(value)); } catch { /* Storage is optional. */ }
}
export function appearanceTokens(value: Appearance): Record<string, string> {
  return {
    '--focus': value.primary, '--secondary': value.secondary,
    '--on-accent': accentText(value.primary), '--on-secondary': accentText(value.secondary),
    '--accent-hover-toward': accentText(value.primary) === '#ffffff' ? '#000000' : '#ffffff',
    '--accent-ink-dark': readableAccent(value.primary, '#232427', '#ffffff'),
    '--accent-ink-light': readableAccent(value.primary, '#e5e6e7', '#000000'),
    '--secondary-ink-dark': readableAccent(value.secondary, '#232427', '#ffffff'),
    '--secondary-ink-light': readableAccent(value.secondary, '#e5e6e7', '#000000'),
  };
}
export function resolvedTheme(theme: Theme, systemLight: boolean): 'dark' | 'light' {
  return theme === 'system' ? systemLight ? 'light' : 'dark' : theme;
}
export function applyAppearance(value: Appearance): void {
  const root = document.documentElement;
  root.dataset.theme = resolvedTheme(value.theme, matchMedia('(prefers-color-scheme: light)').matches);
  for (const [key, color] of Object.entries(appearanceTokens(value))) root.style.setProperty(key, color);
}
