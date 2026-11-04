/**
 * 会话诊断面板（Ctrl+Shift+D）。
 *
 * 解决的问题：用户报"酷狗没识别到""放视频反而识别到了"，但外面完全看不出原因。
 * 这个面板直接把 SMTC 的真实状态摊开：
 *   - 每个会话的 AUMID、PlaybackType（music / video）、有没有时间轴、歌名歌手
 *   - 所有可见窗口的进程名 + 标题（确定酷狗的标题格式和正则对不对）
 *
 * 只在开发/排查时打开，平时不占任何渲染开销。
 */

const inTauri = typeof window !== 'undefined' && !!window.__TAURI_INTERNALS__;

let root = null;
let visible = false;
// 保存时要把用户没在面板里改的字段原样带回去（比如 excludeApps），
// 否则 set_config 会用残缺对象覆盖掉默认排除列表
let lastCfg = null;

function esc(s) {
  return String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
}

function badge(text, tone) {
  return `<span class="diag-badge" data-tone="${tone}">${esc(text)}</span>`;
}

function sessionRow(s) {
  const type = s.playbackType || 'unknown';
  const tone = type === 'video' ? 'warn' : type === 'music' ? 'ok' : 'muted';
  const tl = s.hasTimeline ? badge('有时间轴', 'ok') : badge('无时间轴', 'warn');
  return `
    <div class="diag-row">
      <div class="diag-main">
        <div class="diag-title">${esc(s.title || '（无标题）')}</div>
        <div class="diag-sub">${esc(s.artist || '—')}${s.album ? ' · ' + esc(s.album) : ''}</div>
        <div class="diag-meta">
          ${badge(type, tone)}
          ${tl}
          ${s.playing ? badge('playing', 'ok') : badge('paused', 'muted')}
          ${s.isCurrent ? badge('系统当前', 'accent') : ''}
          <span class="diag-app">${esc(s.appId)}</span>
        </div>
      </div>
      <button class="diag-follow" data-app="${esc(s.appId)}">跟随</button>
    </div>`;
}

function windowRow(w) {
  return `<div class="diag-row diag-row--slim">
    <div class="diag-main">
      <div class="diag-title">${esc(w.title)}</div>
      <div class="diag-sub">${esc(w.process || '（拿不到进程名）')} · ${esc(w.class || '')}</div>
    </div>
  </div>`;
}

function shell() {
  const el = document.createElement('div');
  el.id = 'diag';
  el.hidden = true;
  el.innerHTML = `
    <div class="diag-panel">
      <header class="diag-head">
        <h2>会话诊断</h2>
        <button class="diag-close" title="关闭（Ctrl+Shift+D）">×</button>
      </header>
      <div class="diag-body">
        <section>
          <h3>歌词状态 <span class="diag-hint">区分"源没有"和"真错误"</span></h3>
          <div id="diag-lyric" class="diag-lyric">加载中…</div>
          <div id="diag-timeline-tip" class="diag-tip" hidden></div>
        </section>
        <section>
          <h3>
            候选与得分
            <span class="diag-hint">按当前曲目重新搜一遍，看每个源都返回了什么</span>
          </h3>
          <div class="diag-actions">
            <button id="diag-cands">搜索候选</button>
          </div>
          <div id="diag-candidates" class="diag-list"></div>
        </section>
        <section>
          <h3>SMTC 会话 <span class="diag-hint">PlaybackType=video 的会被自动过滤</span></h3>
          <div id="diag-sessions" class="diag-list">加载中…</div>
        </section>
        <section>
          <h3>可见窗口 <span class="diag-hint">酷狗标题是跑马灯，会滚动；进程名一般是 kugou.exe</span></h3>
          <div id="diag-windows" class="diag-list">加载中…</div>
        </section>
        <section>
          <h3>当前配置</h3>
          <div class="diag-config">
            <label>跟随指定播放器<input id="diag-follow-app" placeholder="留空 = 自动选择"></label>
            <label>窗口回退进程名<input id="diag-wp" placeholder="kugou.exe"></label>
            <label>标题正则<input id="diag-wr" spellcheck="false"></label>
            <label class="diag-inline"><input id="diag-video" type="checkbox"> 允许视频会话</label>
            <label class="diag-inline"><input id="diag-fallback" type="checkbox"> 启用窗口标题回退</label>
            <div class="diag-actions">
              <button id="diag-test">测试正则</button>
              <button id="diag-save" class="primary">保存并应用</button>
            </div>
            <div id="diag-test-out" class="diag-out"></div>
          </div>
        </section>
      </div>
    </div>`;
  return el;
}

/** 歌词状态行：源 / HTTP 码 / 行数 / 错误类型 */
function lyricStatusHtml(st) {
  if (!st) {
    return '<div class="diag-empty">还没有发过歌词请求。播一首歌再回来看。</div>';
  }
  const kindTone =
    st.ok ? 'ok' : st.kind === 'notFound' ? 'warn' : st.kind === 'rateLimited' ? 'warn' : 'err';
  const kindLabel =
    {
      ok: `${st.lineCount} 行`,
      notFound: '源未收录',
      networkError: '网络失败',
      parseError: '解析失败',
      rateLimited: '被限流',
      badRequest: '参数错误',
      allFailed: '全部源失败',
    }[st.kind] || st.kind;

  return `
    <div class="diag-lyric-main">
      <strong>${esc(st.track || '—')}</strong>
      ${badge(st.source || '—', 'muted')}
      ${badge('HTTP ' + (st.httpStatus || '—'), st.httpStatus === 200 ? 'ok' : 'muted')}
      ${badge(kindLabel, kindTone)}
      <span class="diag-app">${st.elapsedMs}ms</span>
    </div>
    <div class="diag-lyric-detail">${esc(st.detail || '')}</div>
    ${attemptsHtml(st.attempts)}`;
}

/**
 * 各源的尝试明细。
 * 这一块回答的是"是源里没有，还是匹配错了"：
 * 每个源单独一行，成功/失败和具体原因都写出来。
 */
function attemptsHtml(attempts) {
  if (!attempts || !attempts.length) return '';
  const rows = attempts
    .map((line) => `<div class="diag-attempt">${esc(line)}</div>`)
    .join('');
  return `<div class="diag-attempts"><div class="diag-attempts-head">各源尝试记录</div>${rows}</div>`;
}

async function refreshLyricStatus() {
  if (!inTauri || !root) return;
  const { invoke } = await import('@tauri-apps/api/core');
  try {
    const st = await invoke('lyrics_status');
    root.querySelector('#diag-lyric').innerHTML = lyricStatusHtml(st);
  } catch (err) {
    root.querySelector('#diag-lyric').textContent = `读取失败：${err}`;
  }
  refreshTimelineTip();
}

/**
 * 无时间轴提示。
 *
 * 酷狗这类播放器把 Position 恒报 0，进度只能靠外推 —— 时间一长必然漂。
 * 与其让用户以为"歌词对不上是 bug"，不如直接告诉他这里可以手动校准。
 */
async function refreshTimelineTip() {
  if (!inTauri || !root) return;
  const box = root.querySelector('#diag-timeline-tip');
  if (!box) return;

  const { invoke } = await import('@tauri-apps/api/core');
  let np = null;
  try {
    np = await invoke('now_playing');
  } catch {
    return;
  }

  const noTimeline = np && np.active && !np.hasTimeline;
  box.hidden = !noTimeline;
  if (noTimeline) {
    box.innerHTML =
      '<b>检测到当前播放器未提供时间轴</b><br>' +
      '可尝试在播放器设置中开启「系统媒体控制 / SMTC」，或使用托盘菜单的「歌词对齐」手动微调。';
  }
}

/**
 * 用当前曲目重新搜一遍候选，把每个源的标题/时长/得分都摊开。
 * 得分是相对值，看的是"第一名和第二名差多少"：
 * 差得越小越可能是匹配错了。
 */
async function searchCandidates() {
  if (!inTauri || !root) return;
  const box = root.querySelector('#diag-candidates');
  const { invoke } = await import('@tauri-apps/api/core');
  box.textContent = '搜索中…';

  try {
    const np = await invoke('now_playing');
    if (!np || !np.active || !np.title) {
      box.innerHTML = '<div class="diag-empty">当前没有正在播放的曲目，无法按曲目搜索。</div>';
      return;
    }
    const cands = await invoke('search_lyrics_candidates', {
      title: np.title,
      artist: np.artist || '',
      album: np.album || null,
      duration: np.durationMs ? np.durationMs / 1000 : null,
    });
    if (!cands.length) {
      box.innerHTML = '<div class="diag-empty">各源都没有候选（不是匹配失败，是源里确实没有）。</div>';
      return;
    }
    box.innerHTML = cands
      .map(
        (c, i) => `
        <div class="diag-cand">
          <span class="diag-cand-rank">${i + 1}</span>
          ${badge(c.source || '—', 'muted')}
          <strong>${esc(c.title || '')}</strong>
          <span class="diag-app">${esc(c.artist || '')}</span>
          <span class="diag-app">${c.duration ? Math.round(c.duration) + 's' : '—'}</span>
          ${c.hasWordSync ? badge('逐字', 'ok') : c.hasSynced ? badge('逐行', 'muted') : ''}
          ${c.hasTranslation ? badge('翻译', 'ok') : ''}
          <span class="diag-cand-score">${c.score ?? '—'}</span>
        </div>`,
      )
      .join('');
  } catch (err) {
    box.textContent = `搜索失败：${err}`;
  }
}

async function refresh() {
  if (!inTauri) {
    root.querySelector('#diag-sessions').textContent = '浏览器环境下没有 SMTC';
    root.querySelector('#diag-windows').textContent = '浏览器环境下没有窗口枚举';
    return;
  }

  const { invoke } = await import('@tauri-apps/api/core');

  try {
    const [sessions, windows, cfg] = await Promise.all([
      invoke('list_sessions'),
      invoke('list_windows'),
      invoke('get_settings'),
    ]);

    const sBox = root.querySelector('#diag-sessions');
    sBox.innerHTML = sessions.length
      ? sessions.map(sessionRow).join('')
      : '<div class="diag-empty">没有任何 SMTC 会话。播放器没开系统媒体支持，或完全没在播。</div>';

    const wBox = root.querySelector('#diag-windows');
    const filtered = windows.filter((w) => !w.process || /kugou|qqmusic|cloudmusic|spotify|foobar|music/i.test(w.process));
    wBox.innerHTML = filtered.length
      ? filtered.map(windowRow).join('')
      : '<div class="diag-empty">没找到疑似音乐播放器的窗口。</div>';

    root.querySelector('#diag-follow-app').value = cfg.followApp || '';
    root.querySelector('#diag-wp').value = cfg.windowProcess || '';
    root.querySelector('#diag-wr').value = cfg.windowRegex || '';
    root.querySelector('#diag-video').checked = !!cfg.allowVideo;
    root.querySelector('#diag-fallback').checked = !!cfg.windowFallback;
    lastCfg = cfg;
  } catch (err) {
    root.querySelector('#diag-sessions').textContent = `读取失败：${err}`;
  }

  await refreshLyricStatus();
}

async function save() {
  if (!inTauri) return;
  const { invoke } = await import('@tauri-apps/api/core');
  const follow = root.querySelector('#diag-follow-app').value.trim();
  // 以完整对象回写：Settings 是整包替换，缺字段会被 serde 用默认值填掉，
  // 那样用户之前调好的 excludeApps / 偏移全会被打回默认
  const next = {
    ...(lastCfg || {}),
    followApp: follow || null,
    windowProcess: root.querySelector('#diag-wp').value.trim(),
    windowRegex: root.querySelector('#diag-wr').value,
    allowVideo: root.querySelector('#diag-video').checked,
    windowFallback: root.querySelector('#diag-fallback').checked,
  };
  await invoke('set_settings', { settings: next });
  const out = root.querySelector('#diag-test-out');
  out.textContent = '已保存，后台会在 500ms 内生效。';
  setTimeout(refresh, 600);
}

function testRegex() {
  const pattern = root.querySelector('#diag-wr').value;
  const out = root.querySelector('#diag-test-out');
  let re;
  try {
    re = new RegExp(pattern);
  } catch (err) {
    out.textContent = `正则编译失败：${err.message}`;
    return;
  }
  const sample = prompt('粘贴一个酷狗窗口标题来测试（例如：晴天 - 周杰伦 - 酷狗音乐）', '晴天 - 周杰伦 - 酷狗音乐');
  if (sample == null) return;
  const m = sample.match(re);
  if (!m) {
    out.textContent = '没匹配上。检查标题里是用 - 还是 – ，以及有没有多余空格。';
    return;
  }
  out.textContent = `title=${m.groups?.title ?? '（无命名分组）'}  artist=${m.groups?.artist ?? '（无）'}`;
}

export function initDiagnostics() {
  root = shell();
  document.body.appendChild(root);

  root.querySelector('.diag-close').addEventListener('click', () => toggle(false));
  root.querySelector('#diag-save').addEventListener('click', save);
  root.querySelector('#diag-test').addEventListener('click', testRegex);
  root.querySelector('#diag-cands').addEventListener('click', searchCandidates);
  root.querySelector('#diag-sessions').addEventListener('click', async (e) => {
    const btn = e.target.closest('.diag-follow');
    if (!btn) return;
    root.querySelector('#diag-follow-app').value = btn.dataset.app;
    await save();
  });

  // Ctrl+Shift+D 打开/关闭 —— 不占全局快捷键，只在页面有焦点时有效
  window.addEventListener('keydown', (e) => {
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'd') {
      e.preventDefault();
      toggle();
    }
    if (e.key === 'Escape' && visible) toggle(false);
  });
}

export async function toggle(next) {
  if (!root) return;
  visible = next === undefined ? !visible : !!next;
  root.hidden = !visible;
  document.body.classList.toggle('diag-open', visible);
  // 告诉主循环"现在别按歌词宽度改窗口"，否则面板一打开就被岛体自适应顶回去
  window.__diagOpen = visible;
  await resizeWindow(visible);
  if (visible) refresh();
}

/**
 * 诊断面板要 560px 宽才放得下，而岛体常态只有 200 来像素。
 * 打开时把窗口临时撑开，关掉再收回去 —— 不收回去岛会挂着一大片空白。
 */
async function resizeWindow(open) {
  if (!inTauri) return;
  try {
    const { getCurrentWindow, LogicalSize } = await import('@tauri-apps/api/window');
    const { invoke } = await import('@tauri-apps/api/core');
    const w = open ? 620 : 420;
    const h = open ? 700 : 64;
    await getCurrentWindow().setSize(new LogicalSize(w, h));
    await invoke('recenter_island', { width: w });
  } catch (err) {
    console.warn('[diag] 调整窗口尺寸失败', err);
  }
}
