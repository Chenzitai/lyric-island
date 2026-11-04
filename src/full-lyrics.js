/**
 * 完整歌词窗口（label: "full-lyrics"）。
 *
 * 用途：想找某一句、或想跳到某个位置时打开它。平时用岛体就够了。
 *
 * 关键交互：
 *   - 当前行高亮并自动居中；
 *   - **用户手动滚动后 3 秒内不抢回滚动位置** —— 这条是刚需，
 *     不然用户往下翻着看，一到下一句就被拽回去，根本看不了；
 *   - 点任意一行 → seek_to。播放器不支持时弹 Toast，不静默失败。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { parseLrc, findLineIndex } from './lyrics/lrc.js';

const USER_SCROLL_GRACE_MS = 3000;

let lines = [];
let translations = [];
let currentIndex = -1;
let clock = { base: 0, at: performance.now(), playing: false };

/** 用户最后一次手动滚动的时间。这一刻之后 3 秒内不自动滚 */
let lastUserScrollAt = 0;
let programmaticScroll = false;

let titleEl = null;
let artistEl = null;
let listEl = null;
let toastEl = null;
let toastTimer = 0;
let rowEls = [];
let supportSeek = false;

function buildDom() {
  const root = document.getElementById('full-root');
  root.innerHTML = '';

  const wrap = document.createElement('div');
  wrap.id = 'fl';

  const header = document.createElement('div');
  header.className = 'fl-header';
  titleEl = document.createElement('div');
  titleEl.className = 'fl-title';
  titleEl.textContent = '没有检测到播放器';
  artistEl = document.createElement('div');
  artistEl.className = 'fl-artist';
  header.append(titleEl, artistEl);

  listEl = document.createElement('div');
  listEl.className = 'fl-list';

  toastEl = document.createElement('div');
  toastEl.className = 'fl-toast';

  wrap.append(header, listEl, toastEl);
  root.appendChild(wrap);

  // 用户手动滚动：滚轮、触摸、滚动条拖动都算。
  // 用 scroll 事件兜底（滚动条拖动不触发 wheel），但要排除程序自身的滚动。
  listEl.addEventListener(
    'scroll',
    () => {
      if (programmaticScroll) return;
      lastUserScrollAt = performance.now();
    },
    { passive: true },
  );
  listEl.addEventListener('wheel', () => {
    lastUserScrollAt = performance.now();
  }, { passive: true });
  listEl.addEventListener('pointerdown', () => {
    lastUserScrollAt = performance.now();
  });
}

function toast(text) {
  if (!toastEl) return;
  toastEl.textContent = text;
  toastEl.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), 2200);
}

/** 重绘列表。歌词变了才调，平时只改 class。 */
function renderList() {
  listEl.innerHTML = '';
  rowEls = [];
  currentIndex = -1;

  const frag = document.createDocumentFragment();
  for (let i = 0; i < lines.length; i++) {
    const row = document.createElement('div');
    row.className = 'fl-row';
    row.dataset.idx = String(i);

    const main = document.createElement('div');
    main.className = 'fl-row-main';
    main.textContent = lines[i].text || '♪';

    row.appendChild(main);

    const tr = translations[i] || '';
    if (tr) {
      const sub = document.createElement('div');
      sub.className = 'fl-row-sub';
      sub.textContent = tr;
      row.appendChild(sub);
    }

    row.addEventListener('click', () => onRowClick(i));
    frag.appendChild(row);
    rowEls.push(row);
  }
  listEl.appendChild(frag);
}

async function onRowClick(i) {
  const target = lines[i]?.time;
  if (typeof target !== 'number') return;

  if (!supportSeek) {
    toast('当前播放器不支持跳转');
    return;
  }

  try {
    await invoke('seek_to', { positionMs: target });
    // 本地时钟立刻跟上，别等下一次 smtc:update
    clock = { base: target, at: performance.now(), playing: clock.playing };
    highlight(i, true);
    toast('已跳到这一句');
  } catch (err) {
    const msg = String(err);
    toast(msg.includes('不支持') ? '当前播放器不支持跳转' : `跳转失败：${msg}`);
  }
}

function highlight(idx, forceScroll) {
  if (idx === currentIndex && !forceScroll) return;
  currentIndex = idx;
  for (let i = 0; i < rowEls.length; i++) {
    const on = i === idx;
    rowEls[i].classList.toggle('active', on);
    rowEls[i].classList.toggle('passed', i < idx);
  }
  if (forceScroll) scrollToRow(idx);
}

function scrollToRow(idx) {
  const row = rowEls[idx];
  if (!row) return;
  const target = row.offsetTop - listEl.clientHeight / 2 + row.offsetHeight / 2;
  programmaticScroll = true;
  listEl.scrollTo({ top: Math.max(0, target), behavior: 'smooth' });
  // smooth 滚动会持续几百毫秒，期间来的 scroll 事件都要当程序滚动
  setTimeout(() => {
    programmaticScroll = false;
  }, 480);
}

function autoScrollIfAllowed(idx) {
  if (performance.now() - lastUserScrollAt < USER_SCROLL_GRACE_MS) return;
  scrollToRow(idx);
}

/** 把 LRC 文本按时间戳对齐（400ms 内算同一句），逻辑和岛体那边一致 */
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
    let lo = 0;
    let hi = parsed.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (parsed[mid].time < t) lo = mid + 1;
      else hi = mid;
    }
    const cur = parsed[lo];
    const prev = lo > 0 ? parsed[lo - 1] : null;
    const best = prev && Math.abs(prev.time - t) <= Math.abs(cur.time - t) ? prev : cur;
    if (best && Math.abs(best.time - t) <= 400) out[i] = best.text;
  }
  return out;
}

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
    if (!res || !res.ok || !res.payload) {
      lines = [];
      translations = [];
      renderList();
      toast('没找到这首歌的歌词');
      return;
    }
    lines = parseLrc(res.payload.synced || '').lines || [];
    translations =
      alignTranslations(lines, res.payload.translated) ||
      alignTranslations(lines, res.payload.romanized);
    renderList();
    // 定位到当前播放位置
    const idx = findLineIndex(lines, clock.base);
    highlight(idx, true);
  } catch (err) {
    console.warn('[full-lyrics] 拉歌词失败', err);
    toast('拉取歌词失败');
  }
}

function tick() {
  requestAnimationFrame(tick);
  if (!lines.length) return;

  const now = performance.now();
  const t = clock.playing ? clock.base + (now - clock.at) : clock.base;
  const idx = findLineIndex(lines, t);
  if (idx === currentIndex) return;
  highlight(idx, false);
  if (idx >= 0) autoScrollIfAllowed(idx);
}

export async function startFullLyrics() {
  document.body.dataset.window = 'full-lyrics';
  buildDom();

  try {
    const p = await invoke('get_current_progress');
    clock = { base: p.positionMs || 0, at: performance.now(), playing: !!p.playing };
  } catch {
    /* ignore */
  }

  await listen('smtc:update', async (e) => {
    const np = e.payload;
    if (!np || !np.active) {
      titleEl.textContent = '没有检测到播放器';
      artistEl.textContent = '';
      return;
    }
    titleEl.textContent = np.title || '未知曲目';
    artistEl.textContent = np.artist || '';
    supportSeek = !!np.hasTimeline && np.durationMs > 0;
    clock = { base: np.positionMs || 0, at: performance.now(), playing: !!np.playing };
    await loadLyricsFor(np);
  });

  await listen('smtc:seek', (e) => {
    const pos = e.payload?.positionMs ?? 0;
    clock = { base: pos, at: performance.now(), playing: clock.playing };
    // seek 是用户主动行为，滚回当前位置是合理的，不算"抢"
    const idx = findLineIndex(lines, pos);
    if (idx >= 0) highlight(idx, true);
  });

  try {
    const np = await invoke('now_playing');
    if (np && np.active) {
      titleEl.textContent = np.title || '未知曲目';
      artistEl.textContent = np.artist || '';
      supportSeek = !!np.hasTimeline && np.durationMs > 0;
      clock = { base: np.positionMs || 0, at: performance.now(), playing: !!np.playing };
      await loadLyricsFor(np);
    }
  } catch {
    /* ignore */
  }

  requestAnimationFrame(tick);
}
