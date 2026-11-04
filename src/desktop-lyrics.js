/**
 * 桌面歌词窗口（label: "desktop-lyrics"）。
 *
 * 和岛体的分工：
 *   - 岛体在屏幕顶上，只有一句，跟着鼠标展开成面板；
 *   - 桌面歌词在桌面上自由摆着，字大、能拖、默认**点不中**（穿透到下层）。
 *
 * 时钟策略：不自己攒，从 `get_current_progress` 拿一次基准，
 * 之后用 rAF 线性外推。这样即使这个窗口是刚打开的（比如从托盘里点开），
 * 也不会从 0 开始滚。
 */

import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseLrc, findLineIndex, buildCharTimeline } from './lyrics/lrc.js';

const win = getCurrentWindow();

let settings = { desktopLyricsLocked: true, desktopLyricsFontSize: 30 };
let lines = [];
let translations = [];
let currentIndex = -2;
let lastActiveChar = -1;
let chars = [];
let charEls = [];

// 时钟：{ base, at, playing }
let clock = { base: 0, at: performance.now(), playing: false };

const rootEl = document.getElementById('desktop-root');
let el = null;
let textEl = null;
let subEl = null;

/** 把 LRC 文本按时间戳对齐到 lines 上（400ms 内算同一句） */
function alignTranslations(lineList, lrcText) {
  if (!lrcText || !lineList.length) return [];
  const parsed = lrcText
    .split(/\r?\n/)
    .map((raw) => {
      const m = raw.match(/^\[(\d+):(\d+(?:\.\d+)?)\](.*)$/);
      if (!m) return null;
      const time = Number(m[1]) * 60000 + Number(m[2]) * 1000;
      const text = m[3].trim();
      return text ? { time, text } : null;
    })
    .filter(Boolean)
    .sort((a, b) => a.time - b.time);

  if (!parsed.length) return [];

  const out = new Array(lineList.length).fill('');
  for (let i = 0; i < lineList.length; i++) {
    const t = lineList[i].time;
    // 二分找最近的翻译行
    let lo = 0;
    let hi = parsed.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (parsed[mid].time < t) lo = mid + 1;
      else hi = mid;
    }
    const cur = parsed[lo];
    const prev = lo > 0 ? parsed[lo - 1] : null;
    const best =
      prev && Math.abs(prev.time - t) <= Math.abs(cur.time - t) ? prev : cur;
    if (best && Math.abs(best.time - t) <= 400) out[i] = best.text;
  }
  return out;
}

function buildDom() {
  rootEl.innerHTML = '';
  el = document.createElement('div');
  el.id = 'dl';
  el.dataset.locked = String(!!settings.desktopLyricsLocked);

  const line = document.createElement('div');
  line.className = 'dl-line';
  textEl = document.createElement('span');
  textEl.className = 'dl-text';
  line.appendChild(textEl);

  subEl = document.createElement('div');
  subEl.className = 'dl-sub';

  // 解锁后才露出来的拖动手柄：给用户一个"这里能抓"的视觉锚点
  const handle = document.createElement('div');
  handle.className = 'dl-handle';
  handle.title = '按住拖动';
  handle.textContent = '⠿';

  el.append(line, subEl, handle);
  rootEl.appendChild(el);

  el.style.setProperty('--dl-font', `${settings.desktopLyricsFontSize}px`);

  // 拖动：只在解锁态生效。锁定态整个窗口是穿透的，根本收不到 pointerdown。
  el.addEventListener('pointerdown', async (e) => {
    if (settings.desktopLyricsLocked) return;
    if (e.button !== 0) return;
    e.preventDefault();
    try {
      await win.startDragging();
      // 拖完把最终位置存下来（逻辑坐标）
      const [pos, scale] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
      await invoke('save_desktop_pos', { x: pos.x / scale, y: pos.y / scale });
    } catch (err) {
      console.warn('[desktop-lyrics] 拖动失败', err);
    }
  });
}

function applySettings(s) {
  settings = { ...settings, ...s };
  if (el) {
    el.dataset.locked = String(!!settings.desktopLyricsLocked);
    el.style.setProperty('--dl-font', `${settings.desktopLyricsFontSize}px`);
  }
}

function setLyrics(newLines, newTranslations) {
  lines = newLines || [];
  translations = newTranslations || [];
  currentIndex = -2;
  lastActiveChar = -1;
  chars = [];
  charEls = [];
  if (textEl) textEl.innerHTML = '';
  if (subEl) subEl.textContent = '';
}

function setIdle() {
  lines = [];
  translations = [];
  currentIndex = -2;
  if (textEl) {
    textEl.className = 'dl-text idle';
    textEl.textContent = '在支持 SMTC 的应用里播放音乐';
  }
  if (subEl) subEl.textContent = '';
}

function renderLine(idx) {
  currentIndex = idx;
  lastActiveChar = -1;
  chars = [];
  charEls = [];

  if (idx < 0) {
    const t = clock.base;
    const first = lines[0];
    const inIntro = !!first && t < first.time;
    textEl.className = inIntro ? 'dl-text intro' : 'dl-text idle';
    textEl.textContent = inIntro ? '♪ 前奏' : '· · ·';
    subEl.textContent = '';
    return;
  }

  const line = lines[idx];
  chars = buildCharTimeline(line);
  const frag = document.createDocumentFragment();
  for (const c of chars) {
    const span = document.createElement('span');
    span.className = 'dl-ch';
    span.textContent = c.ch === ' ' ? '\u00a0' : c.ch;
    frag.appendChild(span);
    charEls.push(span);
  }
  textEl.className = 'dl-text';
  textEl.innerHTML = '';
  textEl.appendChild(frag);

  const tr = translations[idx] || '';
  subEl.textContent = tr;
}

function tick() {
  requestAnimationFrame(tick);
  if (!lines.length) return;

  const now = performance.now();
  const t = clock.playing ? clock.base + (now - clock.at) : clock.base;

  const idx = findLineIndex(lines, t);
  if (idx !== currentIndex) renderLine(idx);

  if (!chars.length) return;

  let active = -1;
  for (let i = 0; i < chars.length; i++) {
    if (t >= chars[i].t0) active = i;
    else break;
  }
  if (active === lastActiveChar) return;

  if (lastActiveChar >= 0) {
    const prev = charEls[lastActiveChar];
    if (prev) {
      prev.classList.remove('active');
      if (t >= chars[lastActiveChar].t1) prev.classList.add('sung');
    }
  }
  const from = Math.min(lastActiveChar + 1, active);
  for (let i = Math.max(0, from); i < active; i++) {
    charEls[i]?.classList.add('sung');
  }
  if (active >= 0) {
    const node = charEls[active];
    if (node) {
      node.classList.add('active');
      if (t >= chars[active].t1) node.classList.add('sung');
    }
  }
  lastActiveChar = active;
}

/** 歌词加载：优先用绑定/多源结果，失败就保持上一份 */
let lastTrackKey = '';

async function loadLyricsFor(np) {
  const key = `${np.title}|${np.artist}`;
  if (key === lastTrackKey) return;
  lastTrackKey = key;

  try {
    const res = await invoke('fetch_lyrics', {
      title: np.title,
      artist: np.artist,
      album: np.album || null,
      duration: np.durationMs > 0 ? np.durationMs / 1000 : null,
    });
    if (!res || !res.ok || !res.payload) return;
    // payload 里是 LRC 原文（synced），不是解析好的行数组
    const parsed = parseLrc(res.payload.synced || '').lines || [];
    const tr =
      alignTranslations(parsed, res.payload.translated) ||
      alignTranslations(parsed, res.payload.romanized);
    setLyrics(parsed, tr);
  } catch (err) {
    console.warn('[desktop-lyrics] 拉歌词失败', err);
  }
}

export async function startDesktopLyrics() {
  document.body.dataset.window = 'desktop-lyrics';
  settings = await invoke('get_settings');
  buildDom();
  applySettings(settings);

  // 初始时钟：从 Rust 拿一次真实进度，不从 0 开始
  try {
    const p = await invoke('get_current_progress');
    clock = { base: p.positionMs || 0, at: performance.now(), playing: !!p.playing };
  } catch {
    /* 没有会话就用默认值 */
  }

  await listen('smtc:update', async (e) => {
    const np = e.payload;
    if (!np || !np.active) {
      setIdle();
      lastTrackKey = '';
      return;
    }
    // 换歌或进度跳变时重新锚定时钟
    clock = { base: np.positionMs || 0, at: performance.now(), playing: !!np.playing };
    await loadLyricsFor(np);
  });

  // seek：后端判定为拖动后发这个事件
  await listen('smtc:seek', (e) => {
    const pos = e.payload?.positionMs ?? 0;
    clock = { base: pos, at: performance.now(), playing: clock.playing };
    currentIndex = -2;
  });

  await listen('settings:changed', (e) => applySettings(e.payload));

  // 补齐一次当前状态（事件可能在前端挂载前就发过了）
  try {
    const np = await invoke('now_playing');
    if (np && np.active) {
      clock = { base: np.positionMs || 0, at: performance.now(), playing: !!np.playing };
      await loadLyricsFor(np);
    } else {
      setIdle();
    }
  } catch {
    setIdle();
  }

  requestAnimationFrame(tick);
}
