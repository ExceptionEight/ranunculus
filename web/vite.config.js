// The web UI: one single-page app; the server serves dist/: an unknown path without an extension is
// index.html. Dev: `npm run dev` proxies /api to the server (SERVER=https://127.0.0.1:52702 by default, its own certificate:
// not checked; the server lets a localhost origin through only from this machine, and a sign-in from it).
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const SERVER = process.env.SERVER || 'https://127.0.0.1:52702'

export default defineConfig({
  plugins: [react()],
  build: { outDir: 'dist', emptyOutDir: true },
  server: { proxy: { '/api': { target: SERVER, changeOrigin: true, secure: false } } },
})
