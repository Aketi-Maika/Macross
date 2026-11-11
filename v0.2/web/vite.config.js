import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// 開発時は Rust サーバ (localhost:8080) へ API と WebSocket を中継する
export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
})
