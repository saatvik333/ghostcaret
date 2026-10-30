import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// Served by Tauri: a fixed port for `tauri dev`, and a build for the
// webviews Tauri 2 targets (WebKitGTK 2.40+, WebView2, WKWebView).
export default defineConfig({
  root: 'ui',
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: ['es2022', 'safari16'],
    modulePreload: { polyfill: false },
    reportCompressedSize: true,
  },
});
