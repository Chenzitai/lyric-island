// 开 CDP 的 Runtime/Log 域，重载页面并收集控制台输出与异常。
// 用法: node cdp-logs.mjs [毫秒]
const waitMs = Number(process.argv[2] || 6000);
const pages = await (await fetch('http://127.0.0.1:9222/json/list')).json();
const page = pages.find((p) => p.type === 'page');
if (!page) {
  console.error('没有找到页面');
  process.exit(1);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
let seq = 0;
const send = (method, params = {}) => ws.send(JSON.stringify({ id: ++seq, method, params }));

ws.addEventListener('open', () => {
  send('Runtime.enable');
  send('Log.enable');
  send('Page.enable');
  setTimeout(() => send('Page.reload', { ignoreCache: true }), 300);
});

ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.method === 'Runtime.consoleAPICalled') {
    const args = (msg.params.args || []).map((a) => a.value ?? a.description ?? a.type).join(' ');
    console.log(`[console.${msg.params.type}]`, args);
  } else if (msg.method === 'Runtime.exceptionThrown') {
    const d = msg.params.exceptionDetails;
    console.log('[exception]', d.text, d.exception?.description || '');
  } else if (msg.method === 'Log.entryAdded') {
    console.log(`[log.${msg.params.entry.level}]`, msg.params.entry.text);
  }
});

setTimeout(() => {
  ws.close();
  process.exit(0);
}, waitMs);
