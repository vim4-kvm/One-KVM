import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'
import assert from 'node:assert/strict'
import { codexInstallCommand } from './src/lib/mcpClientSetup'

// Preserve the standalone heredoc delimiter and final newline for terminal paste.
assert.equal(codexInstallCommand('<url>', '<key>'), [
  "codex mcp remove one-kvm 2>/dev/null; cat >> ~/.codex/config.toml <<'EOF'",
  '',
  '[mcp_servers.one-kvm]',
  'url = "<url>"',
  'http_headers = { Authorization = "Bearer <key>" }',
  'EOF',
  '',
].join('\n'))

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    vue(),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    }
  },
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:8080',
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
  },
})
