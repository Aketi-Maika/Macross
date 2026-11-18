import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// 開発時は Rust サーバ (localhost:8080) へ API と WebSocket を中継する
export default defineConfig({
  plugins: [vue()],
  // 地図（MapLibre）は管理者の地図ページを開いたときだけ読み込む別ファイル
  build: { chunkSizeWarningLimit: 1000 },
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
})
