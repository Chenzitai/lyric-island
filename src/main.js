/**
 * 歌词灵动岛 —— 入口。
 *
 * 数据流：
 *   Rust 线程（SMTC 500ms 轮询）
 *     └─ emit("smtc:update", NowPlaying)
 *          └─ 这里：同步时间锚点 → 拉歌词（切歌时）→ 每帧插值推进高亮
 *
 * 为什么时间要自己插值：
 *   SMTC 的 position 是 500ms 才更新一次的快照，直接用会一顿一顿地跳。
 *   这里记住"那一刻的播放位置 + 本地采样时刻"，两次采样之间用 performance.now()
 *   线性外推，看起来是连续的；每来一个新快照就重新对齐，漂移不会累积。
 */

import { IslandView } from './island.js';
import { extractAccent, applyAccent } from './accent.js';
import { parseLrc } from './lyrics/lrc.js';
import { initDiagnostics, toggle as toggleDiagnostics } from './diagnostics.js';

const inTauri = typeof window !== 'undefined' && !!window.__TAURI_INTERNALS__;
const SETTINGS_MODE = new URLSearchParams(location.search).get('window') === 'settings';
const PICKER_MODE = new URLSearchParams(location.search).get('window') === 'picker';
const DESKTOP_MODE = new URLSearchParams(location.search).get('window') === 'desktop-lyrics';
const FULL_MODE = new URLSearchParams(location.search).get('window') === 'full-lyrics';

const view = new IslandView();

// ---------------------------------------------------------------
// 播放时钟
// ---------------------------------------------------------------
const clock = {
  pos: 0,
  at: performance.now(),
  playing: false,

  sync(posMs, playing) {
    this.pos = posMs || 0;
    this.at = performance.now();
    this.playing = !!playing;
  },

  now() {
    if (!this.playing) return this.pos;
    return this.pos + (performance.now() - this.at);
  },
};

// ---------------------------------------------------------------
// 设置 / 歌词偏移
//
// 三层优先级：perAppOffset[appId] > lyricOffsetMs > 0
// 偏移是"画面时间 = 播放时间 + offset"：正数意味着歌词晚出现。
// ---------------------------------------------------------------
let settings = {
  lyricOffsetMs: 0,
  perAppOffset: {},
  followApp: null,
  excludeApps: [],
  allowVideo: false,
  windowFallback: true,
  windowProcess: 'kugou.exe',
  windowRegex: '',
  islandVisible: true,
  launchAtStartup: false,
  shortcutsEnabled: false,
  showTranslation: true,
  translationSize: 11,
  showProgress: true,
  collapseOnPause: false,
};

let currentAppId = '';

function effectiveOffset() {
  const perApp = settings.perAppOffset || {};
  if (currentAppId && Object.prototype.hasOwnProperty.call(perApp, currentAppId)) {
    return perApp[currentAppId] || 0;
  }
  return settings.lyricOffsetMs || 0;
}

async function loadSettings() {
  if (!inTauri) return;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    settings = await invoke('get_settings');
    // 调试镜像：只在 dev 构建里挂到 window 上（release 会被 terser 删掉）
    if (import.meta.env.DEV) window.__settings = settings;
    view.applySettings(settings);
  } catch (err) {
    console.warn('[settings] 读取失败', err);
  }
}

// ---------------------------------------------------------------
// 歌词缓存与状态
// ---------------------------------------------------------------
const lyricCache = new Map(); // key -> { lines }
let currentTrackKey = '';
// 上一次取过色的封面 URL。用来避免心跳重复解析同一张图
let lastArtwork = '';
let lyricLoading = false;
let lastUpdate = null;
/** 最近一次歌词失败的类型，给 UI 决定文案 */
let lyricFailure = null;

// ---------------------------------------------------------------
// Tauri 窗口
// ---------------------------------------------------------------
let tauriWin = null;
let floating = false;

async function setupWindow() {
  if (!inTauri) {
    view.setResizeHandler(() => {});
    return;
  }

  const { getCurrentWindow, LogicalSize } = await import('@tauri-apps/api/window');
  const { invoke } = await import('@tauri-apps/api/core');
  tauriWin = getCurrentWindow();

  // 尺寸交给 JS 算，位置交给 Rust 算。
  // 原因：@tauri-apps/api 不同小版本里显示器相关方法的位置会变（2.1.x 的
  // Window 上没有 currentMonitor），而 Rust 侧的 current_monitor() 一直稳定。
  view.setResizeHandler(async (w, h) => {
    if (window.__diagOpen) return;
    try {
      await tauriWin.setSize(new LogicalSize(w, h));
      // place_island 而不是 recenter_island：
      // 居中态就重新居中；如果岛体已经被拖到别处（将来做悬浮时），
      // 只在超出屏幕边缘时往内推，不强行拉回中间。
      await invoke('place_island', { width: w });
    } catch (err) {
      console.error('[win] setSize 失败', err);
    }
  });
}

// ---------------------------------------------------------------
// 歌词拉取
// ---------------------------------------------------------------

/** 失败类型 → 给用户看的一句话 */
function failureText(failure) {
  if (!failure) return '暂无歌词';
  switch (failure.kind) {
    case 'notFound':
      return '没找到这首歌的歌词 · 右键托盘「选择歌词…」手动指定';
    case 'networkError':
      return '网络请求失败 · 检查网络后重试';
    case 'parseError':
      return '歌词解析失败 · 打开诊断查看详情';
    case 'rateLimited':
      return '歌词源限流了 · 稍后再试';
    case 'badRequest':
      return '歌曲信息不全，无法搜索歌词';
    default:
      return failure.message || '歌词获取失败';
  }
}

/**
 * 把翻译/音译的 LRC 按时间戳对齐到正文行。
 *
 * 两边的时间戳不是逐行一一对应的（有的源翻译少一句、多一句），
 * 所以不能按下标硬套，必须按时间就近匹配。400ms 内才算同一句，
 * 超过就留空 —— 宁可少一行翻译，也不能把上一句的意思安到这一句上。
 */
function alignByTime(lines, lrcText) {
  if (!lrcText || !lrcText.trim()) return null;
  const { lines: tLines } = parseLrc(lrcText);
  const usable = tLines.filter((l) => l.text && l.text.trim());
  if (!usable.length) return null;

  return lines.map((line) => {
    let lo = 0;
    let hi = usable.length - 1;
    let best = -1;
    let bestDiff = Infinity;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const diff = Math.abs(usable[mid].time - line.time);
      if (diff < bestDiff) {
        bestDiff = diff;
        best = mid;
      }
      if (usable[mid].time < line.time) lo = mid + 1;
      else hi = mid - 1;
    }
    return best >= 0 && bestDiff <= 400 ? usable[best].text : '';
  });
}

async function loadLyrics(track, force) {
  const key = `${track.title}\u0001${track.artist}`;
  if (!force && key === currentTrackKey) return;

  currentTrackKey = key;
  lyricFailure = null;

  const cached = lyricCache.get(key);
  if (cached) {
    applyLyrics(cached.lines, cached.translations);
    return;
  }

  if (!track.title || lyricLoading) return;

  lyricLoading = true;
  // 注意：这里**不清空**已有歌词。切歌瞬间新歌词还没到，
  // 清空会闪一下空白；真正的清空发生在 setLyrics([]) 之外的地方。
  view.lineSubEl.textContent = '正在查找歌词…';

  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const res = await invoke('fetch_lyrics', {
      title: track.title,
      artist: track.artist || '',
      album: track.album || null,
      duration: track.durationMs ? track.durationMs / 1000 : null,
    });

    // 新接口：{ ok, payload, failure }
    if (!res || !res.ok) {
      lyricFailure = res?.failure || { kind: 'allFailed', message: '歌词获取失败' };
      // 失败不清空歌词：显示明确状态，而不是空白
      if (!view.lines.length) {
        view.setLyrics([]);
      }
      view.lineSubEl.textContent = failureText(lyricFailure);
      console.warn('[lyrics] 失败', lyricFailure);
      return;
    }

    const payload = res.payload;
    let lines = [];
    if (payload.synced) {
      lines = parseLrc(payload.synced).lines;
    }
    if (!lines.length && payload.plain) {
      lines = spreadPlainLyrics(payload.plain, track.durationMs);
    }

    if (!lines.length) {
      lyricFailure = { kind: 'parseError', message: '解析后没有可用的行' };
      view.lineSubEl.textContent = failureText(lyricFailure);
      return;
    }

    // 翻译优先，没有就退到音译；两者都没有就是 null
    const translations =
      alignByTime(lines, payload.translated) || alignByTime(lines, payload.romanized);

    lyricCache.set(key, { lines, translations });
    applyLyrics(lines, translations);
  } catch (err) {
    lyricFailure = { kind: 'networkError', message: String(err) };
    view.lineSubEl.textContent = failureText(lyricFailure);
    console.warn('[lyrics]', err);
  } finally {
    lyricLoading = false;
  }
}

function applyLyrics(lines, translations) {
  view.setLyrics(lines, translations || null);
  if (!lines.length) view.lineSubEl.textContent = failureText(lyricFailure);
}

/** 纯文本歌词 → 按总时长均分成假时间轴 */
function spreadPlainLyrics(text, durationMs) {
  const raw = text.split('\n').map((s) => s.trim()).filter(Boolean);
  if (!raw.length) return [];

  const total = durationMs > 0 ? durationMs : raw.length * 4000;
  const per = total / raw.length;

  return raw.map((line, i) => ({
    time: i * per,
    end: (i + 1) * per - 120,
    text: line,
  }));
}

// ---------------------------------------------------------------
// SMTC 事件
// ---------------------------------------------------------------
async function setupSmtc() {
  if (!inTauri) {
    mockTrack();
    return;
  }

  const { listen } = await import('@tauri-apps/api/event');
  const { invoke } = await import('@tauri-apps/api/core');

  await listen('smtc:update', (event) => handleNowPlaying(event.payload));

  // 设置变了：歌词偏移、跟随策略、面板显示项都可能变
  await listen('settings:changed', (event) => {
    settings = event.payload;
    // 调试镜像：只在 dev 构建里挂到 window 上（release 会被 terser 删掉）
    if (import.meta.env.DEV) window.__settings = settings;
    view.applySettings(settings);
  });

  // 托盘 / 快捷键改偏移：立刻让渲染层知道，不用等整包广播
  await listen('lyric:offset', async () => {
    try {
      settings = await invoke('get_settings');
      // 调试镜像：只在 dev 构建里挂到 window 上（release 会被 terser 删掉）
    if (import.meta.env.DEV) window.__settings = settings;
    } catch (err) {
      console.warn('[lyrics] 刷新偏移失败', err);
    }
  });

  // 第一次关窗口时 Rust 会推这一条，然后等 2.5 秒再隐藏窗口。
  // 用内联提示而不是只靠系统通知，是因为 toast 在未打包/无
  // AppUserModelID 的环境里经常静默失败。
  await listen('island:hint', (event) => showHint(event.payload));

  // 选词窗口绑定了新的歌词：丢掉这首歌的缓存并重拉。
  // 必须强刷，否则会拿到刚才那份错的内存缓存。
  await listen('lyric:rebind', async () => {
    if (!lastUpdate) return;
    lyricCache.delete(`${lastUpdate.title}\u0001${lastUpdate.artist}`);
    await loadLyrics(lastUpdate, true);
  });

  // 用户拖了进度条：Rust 已经判定完并给出新位置，把本地时钟直接钉到那里。
  // 没有这一步，前端会沿着旧锚点继续外推，歌词和画面对不上。
  await listen('smtc:seek', (event) => {
    const pos = event.payload?.positionMs;
    if (typeof pos !== 'number') return;
    clock.sync(pos, clock.playing);
    view.tick(clock.now() + effectiveOffset());
    console.info(`[clock] 跟随 seek → ${(pos / 1000).toFixed(1)}s`);
  });

  // 托盘「歌词对齐」：无时间轴的播放器（酷狗）只能这样手动校准
  await listen('lyric:align', async (event) => {
    const p = event.payload || {};

    if (typeof p.deltaMs === 'number' && p.deltaMs !== 0) {
      await applyOffset((settings.lyricOffsetMs || 0) + p.deltaMs);
      return;
    }

    const lines = view.lines || [];
    if (!lines.length) return;

    const t = clock.now() + effectiveOffset();
    const idx = findLineIndex(lines, t);
    const target = p.mode === 'prev' ? lines[idx - 1] : lines[idx + 1];
    if (!target) return;

    // 把偏移调成"让目标行正好成为当前行"
    await applyOffset((settings.lyricOffsetMs || 0) + Math.round(target.time - t));
  });

  // 主动拉一次：后台线程的第一次推送很可能发生在这行之前，
  // 只靠 listen 会一直空白到下次切歌
  try {
    const np = await invoke('now_playing');
    if (np) handleNowPlaying(np);
  } catch (err) {
    console.warn('[smtc] 首次拉取失败', err);
  }
}

/** 改全局歌词偏移并立刻提示 */
async function applyOffset(ms) {
  const value = Math.round(ms);
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const next = await invoke('patch_settings', { patch: { lyricOffsetMs: value } });
    settings = next;
    if (import.meta.env.DEV) window.__settings = next;
    showHint(`歌词偏移 ${value > 0 ? '+' : ''}${value} ms`);
    // 偏移变了，强制下一帧重新定位到正确的行
    view.tick(clock.now() + effectiveOffset());
  } catch (err) {
    console.warn('[align] 改偏移失败', err);
  }
}

/** 处理一帧播放状态（事件推送和主动拉取走同一条路） */
// ---- 一次性提示条（关窗口 → 最小化到托盘）------------------------------
// 盖在岛体上层显示 2.4 秒。之所以不直接改歌词行，是因为 frame() 每帧都会
// 重写 .line-text，提示会被立刻刷掉。
let hintTimer = 0;
function showHint(text) {
  const island = document.getElementById('island');
  if (!island || !text) return;

  let el = document.getElementById('hint-bar');
  if (!el) {
    el = document.createElement('div');
    el.id = 'hint-bar';
    island.appendChild(el);
  }
  el.textContent = text;
  el.classList.add('show');
  island.setAttribute('data-hint', 'true');

  clearTimeout(hintTimer);
  hintTimer = setTimeout(() => {
    el.classList.remove('show');
    island.removeAttribute('data-hint');
  }, 2400);
}

function handleNowPlaying(np) {
  // 演示模式只在 dev 构建里存在；release 里 `import.meta.env.DEV` 是字面量 false，
  // 整个判断会被 terser 折叠掉，不会留下 window.__demoMode 的读取
  if (import.meta.env.DEV && window.__demoMode) return;

  lastUpdate = np;
  // 收到推送的计数，只用于 dev 调试（release 里被折叠掉）
  if (import.meta.env.DEV) window.__smtcCount = (window.__smtcCount || 0) + 1;
  if (import.meta.env.DEV) window.__smtcLast = np;

  if (!np.active) {
    clock.sync(0, false);
    view.setIdle();
    currentTrackKey = '';
    currentAppId = '';
    return;
  }

  if (np.appId) currentAppId = np.appId;

  const key = `${np.title}\u0001${np.artist}`;
  const trackChanged = key !== currentTrackKey;

  view.setActive();
  applyClock(np, trackChanged);
  view.setTrack({
    title: np.title,
    artist: np.artist,
    artwork: np.artwork,
    playing: np.playing,
    positionMs: np.positionMs,
    durationMs: np.durationMs,
    hasTimeline: np.hasTimeline,
  });

  // 封面变了就重新取一次主题色。
  // 用 url 做 key 去重：SMTC 每 2.5 秒心跳都会带同一个 artwork 过来，
  // 不去重的话每 2.5 秒就要解一次图 + 扫 1024 个像素。
  if (np.artwork && np.artwork !== lastArtwork) {
    lastArtwork = np.artwork;
    extractAccent(np.artwork).then((colors) => {
      if (!colors) return; // 黑白封面：保持默认主题色
      applyAccent(colors);
    });
  }

  if (np.title && trackChanged) {
    loadLyrics(np);
  }
}

/**
 * 时钟同步策略 —— 这是"酷狗"这类播放器能不能用的关键。
 *
 * 有时间轴：每帧都用播放器报的 position 校正，最准。
 *
 * 无时间轴（EndTime 恒 0、Position 纹丝不动）：绝对不能每 500ms 都
 * sync(positionMs)。那样等于每半秒把估算进度打回 0，歌词会一直卡在开头、
 * 而且一帧一帧地重头高亮。正确做法是只在"切歌"和"播放/暂停切换"时对表，
 * 其余时候让 performance.now() 自己累加 —— 也就是内部计时器。
 */
function applyClock(np, trackChanged) {
  const prevPlaying = clock.playing;

  if (np.hasTimeline) {
    clock.sync(np.positionMs, np.playing);
    return;
  }

  const restart = trackChanged || prevPlaying !== np.playing;
  if (restart) {
    clock.sync(np.positionMs || 0, np.playing);
    if (trackChanged) {
      const src = np.timelineSource === 'window' ? '窗口标题' : 'SMTC';
      console.info(
        `[clock] ${src}源不提供时间轴（${np.playbackType || 'unknown'}），` +
          '已切换为内部计时器估算进度；手动拖动进度条后需要等切歌或暂停再播才会重新对齐'
      );
    }
  } else {
    clock.playing = np.playing;
  }
}

function mockTrack() {
  const demo = `[00:00.00]（演示模式 · 未在 Tauri 中运行）
[00:03.00]夜色沉进铁磁流体的表面
[00:07.50]每一道波纹都在跟着低频起伏
[00:12.00]高音划过去的时候
[00:15.20]整个球体亮成一枚蓝色的心脏
[00:20.00]· · ·
[00:23.00]把这段歌词当作调试用的占位
[00:27.50]真实歌词来自 LRCLIB`;

  view.setActive();
  view.setTrack({
    title: 'Demo Track',
    artist: 'Browser Preview',
    artwork: '',
    playing: true,
    positionMs: 0,
    durationMs: 30000,
    hasTimeline: true,
  });
  applyLyrics(parseLrc(demo).lines);
  clock.sync(0, true);

  setInterval(() => {
    const t = clock.now();
    if (t > 31000) clock.sync(0, true);
  }, 1000);
}

// ---------------------------------------------------------------
// 调试入口（仅 dev 构建）
// ---------------------------------------------------------------
// `import.meta.env.DEV` 在 `vite build` 时会被静态替换成 false，
// 整块连同里面的函数引用一起被 terser 删掉 —— release 包里
// 不会有 __demo / __api / __view 这些挂在 window 上的东西。
//
// 对应的 Rust 侧 debug_* 命令也用 `#[cfg(debug_assertions)]` 摘掉了，
// 所以即使有人在 release 里手敲 invoke('debug_emit_seek') 也只会得到
// "命令不存在"。
if (import.meta.env.DEV) {
  window.__demo = () => {
    window.__demoMode = true;
    mockTrack();
  };
  window.__demoOff = () => {
    window.__demoMode = false;
  };
  window.__view = view;

  // CDP 调试入口：直接拿到 invoke 和取色函数，
  // 省得每次去猜 vite 预构建产物的路径（那个路径带 hash，会变）。
  // 注意 invoke 是动态导入的（顶层没有 import 它），所以这里也用动态导入，
  // 否则会抛 ReferenceError 把后面所有调试入口一起打断。
  if (inTauri) {
    import('@tauri-apps/api/core').then((m) => {
      window.__api = { invoke: m.invoke };
    });
  }
  window.__accentOf = (url) =>
    extractAccent(url).then((c) => {
      applyAccent(c);
      return c;
    });
  window.__diag = toggleDiagnostics;
  window.__retryLyrics = () => {
    if (lastUpdate) loadLyrics(lastUpdate, true);
  };
  window.__lyricState = () => ({ failure: lyricFailure, key: currentTrackKey });
  window.__align = (ms) => applyOffset(ms);
  window.__progress = async () => {
    const { invoke } = await import('@tauri-apps/api/core');
    return invoke('get_current_progress');
  };
}

// ---------------------------------------------------------------
// 每帧循环
// ---------------------------------------------------------------
function frame() {
  // 偏移在这里加：画面时间 = 播放时间 + offset
  const t = clock.now() + effectiveOffset();
  view.tick(t);
  requestAnimationFrame(frame);
}

// ---------------------------------------------------------------
// 控制按钮
// ---------------------------------------------------------------
// ---------------------------------------------------------------
// 进度条 seek（拖拽 / 点击）
//
// Rust 侧 TryChangePlaybackPositionAsync 对不支持的播放器返回 false，
// 命令会带着 "当前播放器不支持进度跳转" 报回来 —— 这里把两件事区分开：
//   - 播放器拒绝 → 提示"不支持"，这是它的能力问题
//   - 其它异常   → 提示"失败"，这是我们这边的问题
// ---------------------------------------------------------------
view.onSeek = async (ms) => {
  if (!inTauri) return;
  const target = Math.max(0, Math.round(ms));
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('seek_to', { positionMs: target });
    // 拖动后的位置立刻成为新的时钟锚点，否则要等下一次 seek 事件才追平
    clock.sync(target, clock.playing);
    view.tick(clock.now() + effectiveOffset());
  } catch (err) {
    const msg = String(err);
    console.warn('[seek] 失败', err);
    showHint(msg.includes('不支持') ? '当前播放器不支持进度跳转' : '进度跳转失败');
  }
};

view.onSeekBlocked = () => {
  showHint('当前播放器不支持进度跳转');
};

view.onOpenSettings = async () => {
  view.setExpanded(false);
  if (!inTauri) return;
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('open_settings');
  } catch (err) {
    console.warn('[settings] 打开失败', err);
  }
};

// 播放队列还没做。点它只给一条提示，不要假装打开别的窗口 ——
// 用户点"歌单"却弹设置，会以为点错了。
view.onOpenPlaylist = async () => {
  showHint('播放队列还在做，先用完整歌词（托盘菜单）');
};

view.onControl = async (action) => {
  if (!inTauri) {
    console.log('[control]', action);
    return;
  }
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const map = {
      toggle: 'toggle',
      next: 'next',
      previous: 'previous',
      play: 'play',
      pause: 'pause',
    };
    await invoke('send_control', { action: map[action] || 'toggle' });
  } catch (err) {
    console.warn('[control] 失败', err);
  }
};

// ---------------------------------------------------------------
// 启动
// ---------------------------------------------------------------
(async function boot() {
  if (PICKER_MODE) {
    const { startLyricPicker } = await import('./lyric-picker.js');
    await startLyricPicker();
    return;
  }

  if (DESKTOP_MODE) {
    const { startDesktopLyrics } = await import('./desktop-lyrics.js');
    await startDesktopLyrics();
    return;
  }

  if (FULL_MODE) {
    const { startFullLyrics } = await import('./full-lyrics.js');
    await startFullLyrics();
    return;
  }

  if (SETTINGS_MODE) {
    document.body.dataset.window = 'settings';
    const { startSettingsWindow } = await import('./settings-window.js');
    await startSettingsWindow();
    return;
  }

  initDiagnostics();
  await loadSettings();
  await setupWindow();
  await setupSmtc();

  requestAnimationFrame(frame);

  if (inTauri) {
    window.addEventListener('dblclick', async () => {
      try {
        floating = !floating;
        document.body.dataset.floating = String(floating);
        if (floating) {
          await tauriWin.setAlwaysOnTop(true);
        }
      } catch (err) {
        console.warn(err);
      }
    });
  }
})();
