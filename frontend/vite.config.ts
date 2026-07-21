import { defineConfig } from 'vite'
import solid from 'vite-plugin-solid'

export default defineConfig({
  plugins: [solid()],
  build: {
    sourcemap: true,
    // Without this the minifier rewrites breakpoints to media-query range
    // syntax (`@media (width<=860px)`), which Safari ignores before 16.4 —
    // every breakpoint silently stops matching and phones get the desktop grid.
    cssTarget: 'safari14',
  },
  server: {
    host: '127.0.0.1',
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
        ws: true,
      },
      '/xrpc': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
        ws: true,
      },
      '/.well-known': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
      },
      '/client-metadata.json': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
      },
    },
  },
  define: {
    'import.meta.env.VITE_API_BASE': JSON.stringify(process.env.VITE_API_BASE ?? ''),
    'import.meta.env.VITE_BASE_URL': JSON.stringify(process.env.VITE_BASE_URL ?? ''),
    'import.meta.env.VITE_OAUTH_CLIENT_ID': JSON.stringify(process.env.VITE_OAUTH_CLIENT_ID ?? ''),
    'import.meta.env.VITE_OAUTH_REDIRECT_URI': JSON.stringify(process.env.VITE_OAUTH_REDIRECT_URI ?? ''),
    'import.meta.env.VITE_OAUTH_SCOPE': JSON.stringify(process.env.VITE_OAUTH_SCOPE ?? ''),
    'import.meta.env.VITE_RADIO_SERVICE_DID': JSON.stringify(process.env.VITE_RADIO_SERVICE_DID ?? ''),
    'import.meta.env.VITE_RADIO_SERVICE_ID': JSON.stringify(process.env.VITE_RADIO_SERVICE_ID ?? ''),
    'import.meta.env.VITE_SYNDICATION_WORKER_BASE': JSON.stringify(process.env.VITE_SYNDICATION_WORKER_BASE ?? ''),
  },
})
