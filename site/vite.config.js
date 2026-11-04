import { resolve } from 'node:path';
import { defineConfig } from 'vite';

// The landing page plus the comparison, download, policy and not-found pages, each its own HTML file.
const pages = ['index', 'why', 'download', 'privacy', 'terms', '404'];

export default defineConfig({
  build: {
    rollupOptions: {
      input: Object.fromEntries(pages.map((p) => [p, resolve(import.meta.dirname, `${p}.html`)])),
    },
  },
});
