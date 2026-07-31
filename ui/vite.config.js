import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: 'es2022',
    sourcemap: true,
  },
  // Under vitest, resolve svelte's browser entry (index-client) instead of
  // the server one so component `mount()` works in happy-dom tests.
  resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
  test: {
    environment: 'happy-dom',
    include: ['src/**/*.test.{js,mjs}'],
  },
});
