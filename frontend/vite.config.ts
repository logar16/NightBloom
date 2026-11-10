import { readFileSync } from 'node:fs';
import { defineConfig, transformWithOxc } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig(({ mode }) => ({
  plugins: [react(), {
    name: 'appearance-before-paint',
    async transformIndexHtml() {
      const source = readFileSync(new URL('./src/theme/appearance.ts', import.meta.url), 'utf8').replace(/^export /gm, '');
      const { code } = await transformWithOxc(`${source}\napplyAppearance(readAppearance());`, 'appearance.ts');
      return [{ tag: 'script', children: `(()=>{${code}})();`, injectTo: 'head-prepend' }];
    },
  }],
  build: { chunkSizeWarningLimit: 1000 },
  server: {
    port: 5173,
    proxy: {
      '/api': process.env.ASTROIMP_DEV_API
        ?? `http://127.0.0.1:${mode === 'desktop' ? 8766 : 8765}`,
    },
  },
}));
