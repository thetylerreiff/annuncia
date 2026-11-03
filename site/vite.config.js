import { resolve } from 'node:path';
import { defineConfig } from 'vite';

// The landing page plus the policy pages, each its own HTML file.
const pages = ['index', 'privacy', 'terms'];

export default defineConfig({
  build: {
    rollupOptions: {
      input: Object.fromEntries(pages.map((p) => [p, resolve(import.meta.dirname, `${p}.html`)])),
    },
  },
});
