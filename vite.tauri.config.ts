import { resolve } from 'node:path'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The renderer for the Tauri app (app-rs/crates/desktop): src/renderer as electron-vite builds
// it, with src/tauri/bridge.ts standing in for the preload.
export default defineConfig({
  root: resolve(__dirname, 'src/tauri'),
  base: './',
  resolve: { alias: { '@shared': resolve(__dirname, 'src/shared') } },
  plugins: [react()],
  server: { port: 5174, strictPort: true },
  build: { outDir: resolve(__dirname, 'out/tauri-renderer'), emptyOutDir: true }
})
