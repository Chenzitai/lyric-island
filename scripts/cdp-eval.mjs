// 通过 CDP 执行一段表达式，用来诊断页面实际状态。
// 用法: node cdp-eval.mjs "<表达式>"
//       node cdp-eval.mjs --file scripts/xxx.expr.js
//       node cdp-eval.mjs --port 9333 --url test-audio "<表达式>"
// 默认连 Tauri 的 WebView2（9222）；--port 可以指向任意 Chromium 调试端口。
import { readFile } from 'node:fs/promises';

const argv = process.argv.slice(2);
function takeFlag(name, fallback) {
  const i = argv.indexOf(name);
  if (i < 0) return fallback;
  const v = argv[i + 1];
  argv.splice(i, 2);
  return v;
}

const port = takeFlag('--port', '9222');
const urlMatch = takeFlag('--url', '');

const listRes = await fetch(`http://127.0.0.1:${port}/json/list`);
const pages = await listRes.json();
const page = urlMatch
  ? pages.find((p) => p.type === 'page' && p.url.includes(urlMatch))
  : pages.find((p) => p.type === 'page');
if (!page) {
  console.error('没有找到页面');
  process.exit(1);
}

const expr =
  argv[0] === '--file'
    ? await readFile(argv[1], 'utf8')
    : argv[0] || 'document.title';
const ws = new WebSocket(page.webSocketDebuggerUrl);

const timeout = setTimeout(() => {
  console.error('CDP 超时');
  process.exit(1);
}, 15000);

ws.addEventListener('open', () => {
  ws.send(
    JSON.stringify({
      id: 1,
      method: 'Runtime.evaluate',
      params: { expression: expr, returnByValue: true, awaitPromise: true },
    })
  );
});

ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id !== 1) return;
  clearTimeout(timeout);
  const res = msg.result?.result;
  if (msg.result?.exceptionDetails) {
    console.log('EXCEPTION:', JSON.stringify(msg.result.exceptionDetails.text));
    console.log(JSON.stringify(msg.result.exceptionDetails.exception?.description || ''));
  } else {
    console.log(typeof res?.value === 'string' ? res.value : JSON.stringify(res?.value, null, 2));
  }
  ws.close();
  process.exit(0);
});

ws.addEventListener('error', (e) => {
  console.error('WS 错误', e.message || e);
  process.exit(1);
});
