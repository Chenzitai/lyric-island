// 通过 CDP 直接截取 WebView2 内部渲染结果（不经过屏幕合成）。
// 用来区分"页面没画出来"和"画出来了但窗口合成没显示"。
// 用法: node cdp-shot.mjs <输出路径>
import { writeFileSync } from 'node:fs';

const out = process.argv[2] || 'cdp-shot.png';
const pages = await (await fetch('http://127.0.0.1:9222/json/list')).json();
const page = pages.find((p) => p.type === 'page');
if (!page) {
  console.error('没有找到页面');
  process.exit(1);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
const timeout = setTimeout(() => {
  console.error('超时');
  process.exit(1);
}, 10000);

ws.addEventListener('open', () => {
  // 透明背景需要在截图时带上 alpha
  ws.send(
    JSON.stringify({
      id: 1,
      method: 'Page.captureScreenshot',
      params: { format: 'png', captureBeyondViewport: false, fromSurface: true },
    })
  );
});

ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id !== 1) return;
  clearTimeout(timeout);
  if (msg.result?.data) {
    writeFileSync(out, Buffer.from(msg.result.data, 'base64'));
    console.log('saved', out, Buffer.from(msg.result.data, 'base64').length, 'bytes');
  } else {
    console.log('no data', JSON.stringify(msg));
  }
  ws.close();
  process.exit(0);
});
