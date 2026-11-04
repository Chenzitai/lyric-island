import { defineConfig } from 'vite';

// Tauri 用的是自定义协议加载 dist，所以 base 必须是相对路径
export default defineConfig({
  base: './',
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
    watch: {
      // src-tauri 里的东西变了不该触发前端 HMR
      // *.tmpdir 是编辑器"写临时文件再原子替换"时的中间目录，
      // 监听它会在 Windows 上撞到 EBUSY（文件还被占用）把整个 dev server 弄崩
      ignored: ['**/src-tauri/**', '**/*.tmpdir/**', '**/.*.tmpdir/**'],
    },
  },
  build: {
    target: 'chrome110',
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
  },
});
