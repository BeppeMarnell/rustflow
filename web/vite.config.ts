import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { defineConfig, type ProxyOptions } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const keyFile = process.env.CHATFLOW_API_KEY_FILE || fileURLToPath(new URL('../.local/api-key', import.meta.url));

function apiProxy(): ProxyOptions {
  return {
    target: process.env.CHATFLOW_API_URL || 'http://127.0.0.1:3000',
    changeOrigin: true,
    configure(proxy) {
      proxy.on('proxyReq', (request) => {
        const key = readFileSync(keyFile, 'utf8').trim();
        request.setHeader('Authorization', `Bearer ${key}`);
      });
    }
  };
}

export default defineConfig({
  plugins: [svelte()],
  server: {
    host: '127.0.0.1',
    proxy: { '/v1': apiProxy(), '/health': apiProxy() }
  },
  preview: {
    host: '127.0.0.1',
    proxy: { '/v1': apiProxy(), '/health': apiProxy() }
  }
});
