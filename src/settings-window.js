/**
 * 设置窗口（独立窗口，label = "settings"）。
 *
 * 和岛体共用同一个 index.html，靠 `?window=settings` 分流 —— 这样
 * 构建产物只有一套，打包体积不翻倍。
 *
 * 所有读写都走 Rust 命令，前端不直接碰 store：
 * 开关一改就 save，因为这东西随时会被任务管理器杀掉。
 */

const invoke = async (cmd, args) => {
  const { invoke: inv } = await import('@tauri-apps/api/core');
  return inv(cmd, args);
};

let S = null; // 当前设置
let saveTimer = 0;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

function section(title, hint) {
  const s = el('section', 'st-section');
  const h = el('h3', 'st-h');
  h.textContent = title;
  if (hint) {
    const sp = el('span', 'st-hint', hint);
    h.appendChild(sp);
  }
  s.appendChild(h);
  return s;
}

function row(label, hint) {
  const r = el('label', 'st-row');
  const l = el('span', 'st-label', label);
  if (hint) l.title = hint;
  r.appendChild(l);
  return r;
}

function toggleEl(checked, onChange) {
  const wrap = el('span', 'st-toggle');
  const input = document.createElement('input');
  input.type = 'checkbox';
  input.checked = !!checked;
  input.addEventListener('change', () => onChange(input.checked));
  const knob = el('span', 'st-knob');
  wrap.appendChild(input);
  wrap.appendChild(knob);
  return wrap;
}

function textEl(value, placeholder, onChange) {
  const input = el('input', 'st-input');
  input.type = 'text';
  input.value = value ?? '';
  input.placeholder = placeholder || '';
  input.addEventListener('change', () => onChange(input.value));
  return input;
}

function numberEl(value, onChange) {
  const input = el('input', 'st-input st-input--num');
  input.type = 'number';
  input.step = '50';
  input.value = String(value ?? 0);
  input.addEventListener('change', () => onChange(parseInt(input.value, 10) || 0));
  return input;
}

/** 改了就排队保存，300ms 合并一次 */
function scheduleSave(patch) {
  S = { ...S, ...patch };
  clearTimeout(saveTimer);
  saveTimer = setTimeout(flushSave, 300);
}

/**
 * 立刻把当前 S 写进 settings.json。
 *
 * 为什么需要"立刻"这一档：300ms 的防抖在正常操作下够用，
 * 但用户完全可能改完开关 200ms 内就点窗口关闭 / 退出应用 ——
 * 那时候定时器还没跑，改动就丢了。所以窗口失焦和页面卸载时各补一次。
 */
let saving = null;
async function flushSave() {
  clearTimeout(saveTimer);
  saveTimer = 0;
  // 串行化：失焦和定时器可能同时触发，别让两次 set_settings 互相覆盖
  if (saving) return saving;
  saving = (async () => {
    try {
      const saved = await invoke('set_settings', { settings: S });
      S = saved;
      flash('已保存');
    } catch (e) {
      flash(`保存失败：${e}`, true);
    } finally {
      saving = null;
    }
  })();
  return saving;
}

window.addEventListener('blur', () => {
  if (saveTimer) flushSave();
});
window.addEventListener('beforeunload', () => {
  if (saveTimer) flushSave();
});

function flash(msg, isErr) {
  const box = document.getElementById('st-toast');
  if (!box) return;
  box.textContent = msg;
  box.dataset.err = String(!!isErr);
  box.classList.add('show');
  clearTimeout(flash._t);
  flash._t = setTimeout(() => box.classList.remove('show'), 1600);
}

// ============================================================
// 各分区
// ============================================================

function buildGeneral(mount) {
  const s = section('通用', '开机自启走 Rust 侧插件，启动时会用系统真实状态回读');

  const r1 = row('开机自启');
  r1.appendChild(
    toggleEl(S.launchAtStartup, async (on) => {
      try {
        const actual = await invoke('set_autostart', { enabled: on });
        S.launchAtStartup = actual;
        // 命令已经把设置写回 store 并广播，这里只刷新勾选态
        const input = r1.querySelector('input');
        if (input) input.checked = actual;
        flash(actual ? '已开启开机自启' : '已关闭开机自启');
      } catch (e) {
        const input = r1.querySelector('input');
        if (input) input.checked = !on;
        flash(String(e), true);
      }
    })
  );
  s.appendChild(r1);

  const r2 = row('全局快捷键', 'Ctrl+Shift+L 切显隐 / ←→ 调整歌词偏移');
  r2.appendChild(
    toggleEl(S.shortcutsEnabled, async (on) => {
      try {
        const rep = await invoke('set_shortcuts_enabled', { enabled: on });
        S.shortcutsEnabled = on;
        if (rep.failed && rep.failed.length) {
          flash(`这些被占用了：${rep.failed.join('、')}`, true);
        } else {
          flash(on ? '快捷键已注册' : '快捷键已注销');
        }
      } catch (e) {
        flash(String(e), true);
      }
    })
  );
  s.appendChild(r2);

  const r3 = row('语言');
  const sel = el('select', 'st-input');
  sel.innerHTML = '<option>简体中文</option>';
  sel.disabled = true;
  r3.appendChild(sel);
  s.appendChild(r3);

  mount.appendChild(s);
}

/** 桌面歌词分区的设置（跟随播放、锁定、字号） */
function buildDesktopLyrics(mount) {
  const s = section(
    '桌面歌词',
    '跟随播放默认关闭。打开后，检测到开始播放会自动弹出；暂停 3 秒后自动收起'
  );

  const r0 = row('跟随播放自动显示', '只认音乐会话，视频不会触发；自动收放不会改动你的手动开关');
  r0.appendChild(
    toggleEl(S.desktopLyricsFollowPlayback, (on) => {
      S.desktopLyricsFollowPlayback = on;
      scheduleSave({ desktopLyricsFollowPlayback: on });
      flash(on ? '播放时会自动弹出桌面歌词' : '已关闭跟随播放');
    })
  );
  s.appendChild(r0);

  const r1 = row('当前显示', '手动开关，不受「跟随播放」影响');
  r1.appendChild(
    toggleEl(S.desktopLyricsVisible, async (on) => {
      try {
        const v = await invoke('set_desktop_lyrics_visible', { visible: on });
        S.desktopLyricsVisible = v;
        flash(v ? '桌面歌词已显示' : '桌面歌词已隐藏');
      } catch (e) {
        flash(String(e), true);
      }
    })
  );
  s.appendChild(r1);

  const r2 = row('锁定位置', '锁定时鼠标完全穿透，点得到下层软件；解锁后才能拖动');
  r2.appendChild(
    toggleEl(S.desktopLyricsLocked, async (on) => {
      try {
        const v = await invoke('set_desktop_lyrics_locked', { locked: on });
        S.desktopLyricsLocked = v;
        flash(v ? '已锁定（点击穿透）' : '已解锁，可按住 ⠿ 拖动');
      } catch (e) {
        flash(String(e), true);
      }
    })
  );
  s.appendChild(r2);

  const r3 = row('字号（px）');
  r3.appendChild(
    numberEl(S.desktopLyricsFontSize, (v) => {
      const size = Math.min(72, Math.max(16, Math.round(v)));
      S.desktopLyricsFontSize = size;
      invoke('set_desktop_lyrics_font', { size }).catch((e) => flash(String(e), true));
    })
  );
  s.appendChild(r3);

  mount.appendChild(s);
}

function buildLyrics(mount) {
  const s = section('歌词', '偏移优先级：按应用 > 全局 > 0');

  const r1 = row('全局偏移（ms）', '正数 = 歌词延后，负数 = 提前');
  r1.appendChild(numberEl(S.lyricOffsetMs, (v) => scheduleSave({ lyricOffsetMs: v })));
  s.appendChild(r1);

  const r2 = row('按应用偏移', '无时间轴的播放器（酷狗）容易漂，可以单独特调');
  const wrap = el('div', 'st-perapp');
  const listBox = el('div', 'st-perapp-list');
  const renderList = () => {
    listBox.innerHTML = '';
    const entries = Object.entries(S.perAppOffset || {});
    if (!entries.length) {
      listBox.appendChild(el('div', 'st-empty', '还没有按应用偏移'));
    }
    for (const [appId, ms] of entries) {
      const line = el('div', 'st-perapp-row');
      line.appendChild(el('span', 'st-perapp-id', appId));
      line.appendChild(
        numberEl(ms, (v) => {
          const next = { ...(S.perAppOffset || {}), [appId]: v };
          scheduleSave({ perAppOffset: next });
        })
      );
      const del = el('button', 'st-mini', '删');
      del.addEventListener('click', () => {
        const next = { ...(S.perAppOffset || {}) };
        delete next[appId];
        scheduleSave({ perAppOffset: next });
        renderList();
      });
      line.appendChild(del);
      listBox.appendChild(line);
    }
  };
  renderList();
  wrap.appendChild(listBox);

  const addRow = el('div', 'st-perapp-add');
  const idInput = el('input', 'st-input');
  idInput.placeholder = '应用 AUMID，例如 kugou';
  const msInput = el('input', 'st-input st-input--num');
  msInput.type = 'number';
  msInput.placeholder = 'ms';
  const addBtn = el('button', 'st-mini', '添加');
  addBtn.addEventListener('click', () => {
    const id = idInput.value.trim();
    if (!id) return;
    const ms = parseInt(msInput.value, 10) || 0;
    const next = { ...(S.perAppOffset || {}), [id]: ms };
    scheduleSave({ perAppOffset: next });
    idInput.value = '';
    msInput.value = '';
    renderList();
  });
  addRow.append(idInput, msInput, addBtn);
  wrap.appendChild(addRow);
  r2.appendChild(wrap);
  s.appendChild(r2);

  const r3 = row('歌词源优先级');
  r3.appendChild(
    el('span', 'st-muted', '网易云 30 / QQ 音乐 20 / LRCLIB 10，按得分取前三')
  );
  s.appendChild(r3);

  mount.appendChild(s);

  // 悬停面板的显示项，单独分一组，免得和"偏移"混在一起
  const p = section('灵动岛', '悬停展开面板的显示方式');

  const r4 = row('显示翻译 / 音译', '关掉后第二行只显示下一句预览；再关就只剩单行');
  r4.appendChild(toggleEl(S.showTranslation !== false, (v) => scheduleSave({ showTranslation: v })));
  p.appendChild(r4);

  const r5 = row('翻译字号（px）', '9 – 20，默认 11');
  r5.appendChild(
    numberEl(S.translationSize || 11, (v) => scheduleSave({ translationSize: clampNum(v, 9, 20) }))
  );
  p.appendChild(r5);

  const r6 = row('显示进度条', '关掉后展开面板只留封面和控制');
  r6.appendChild(toggleEl(S.showProgress !== false, (v) => scheduleSave({ showProgress: v })));
  p.appendChild(r6);

  const r7 = row('暂停时收起岛体', '收起后只留一行歌词，减少遮挡');
  r7.appendChild(toggleEl(!!S.collapseOnPause, (v) => scheduleSave({ collapseOnPause: v })));
  p.appendChild(r7);

  mount.appendChild(p);
}

function clampNum(v, lo, hi) {
  const n = Number(v);
  if (!Number.isFinite(n)) return lo;
  return Math.max(lo, Math.min(hi, Math.round(n)));
}

function buildFollow(mount) {
  const s = section('跟随', '决定"现在该显示哪个播放器"');

  const r1 = row('跟随指定播放器');
  const sel = el('select', 'st-input');
  sel.innerHTML = '<option value="">自动选择（推荐）</option>';
  sel.value = S.followApp || '';
  r1.appendChild(sel);
  s.appendChild(r1);

  // 会话列表异步填充
  invoke('list_sessions')
    .then((sessions) => {
      for (const x of sessions || []) {
        const o = document.createElement('option');
        o.value = x.appId;
        o.textContent = `${x.appId}${x.title ? ' · ' + x.title : ''}`;
        sel.appendChild(o);
      }
      sel.value = S.followApp || '';
    })
    .catch(() => {});

  sel.addEventListener('change', () => scheduleSave({ followApp: sel.value || null }));

  const r2 = row('排除列表', '一行一个，子串匹配，不区分大小写');
  const ta = el('textarea', 'st-input st-textarea');
  ta.rows = 4;
  ta.value = (S.excludeApps || []).join('\n');
  ta.addEventListener('change', () => {
    const list = ta.value
      .split('\n')
      .map((x) => x.trim())
      .filter(Boolean);
    scheduleSave({ excludeApps: list });
  });
  r2.appendChild(ta);
  s.appendChild(r2);

  const r3 = row('允许视频会话');
  r3.appendChild(toggleEl(S.allowVideo, (v) => scheduleSave({ allowVideo: v })));
  s.appendChild(r3);

  const r4 = row('窗口标题回退', 'SMTC 里没有音乐会话时，直接读窗口标题');
  r4.appendChild(toggleEl(S.windowFallback, (v) => scheduleSave({ windowFallback: v })));
  s.appendChild(r4);

  const r5 = row('回退进程名');
  r5.appendChild(textEl(S.windowProcess, 'kugou.exe', (v) => scheduleSave({ windowProcess: v })));
  s.appendChild(r5);

  const r6 = row('标题正则', '要有 title / artist 两个命名分组');
  const reInput = el('input', 'st-input st-mono');
  reInput.type = 'text';
  reInput.value = S.windowRegex || '';
  reInput.spellcheck = false;
  reInput.addEventListener('change', () => scheduleSave({ windowRegex: reInput.value }));
  r6.appendChild(reInput);
  s.appendChild(r6);

  const testRow = el('div', 'st-actions');
  const testBtn = el('button', 'st-btn', '测试正则');
  const out = el('div', 'st-out');
  testBtn.addEventListener('click', () => {
    let re;
    try {
      re = new RegExp(reInput.value);
    } catch (e) {
      out.textContent = `正则编译失败：${e.message}`;
      return;
    }
    const sample = prompt(
      '粘贴一个窗口标题来测试（酷狗跑马灯示例）',
      ' - Into You - 酷狗音乐 Ariana Grande'
    );
    if (sample == null) return;
    const m = sample.match(re);
    out.textContent = m
      ? `title=${m.groups?.title ?? '（无）'}  artist=${m.groups?.artist ?? '（无）'}`
      : '没匹配上。注意酷狗标题是滚动的，前缀会变。';
  });
  testRow.appendChild(testBtn);
  s.appendChild(testRow);
  s.appendChild(out);

  mount.appendChild(s);
}

function badge(text, tone) {
  return `<span class="diag-badge" data-tone="${tone}">${text}</span>`;
}

async function buildDiagnostics(mount) {
  const s = section('诊断', '排查"为什么没识别到""为什么没歌词"');

  // 歌词状态
  const lyricBox = el('div', 'st-lyric-status');
  lyricBox.innerHTML = '<div class="st-empty">还没有发过歌词请求</div>';
  s.appendChild(lyricBox);

  // 会话
  const sessBox = el('div', 'diag-list');
  sessBox.textContent = '加载中…';
  s.appendChild(el('h4', 'diag-subtitle', 'SMTC 会话'));
  s.appendChild(sessBox);

  // 窗口
  const winBox = el('div', 'diag-list');
  winBox.textContent = '加载中…';
  s.appendChild(el('h4', 'diag-subtitle', '疑似播放器窗口'));
  s.appendChild(winBox);

  const refreshBtn = el('button', 'st-btn', '刷新');
  refreshBtn.addEventListener('click', () => refreshDiagnostics(lyricBox, sessBox, winBox));
  s.appendChild(refreshBtn);

  mount.appendChild(s);
  refreshDiagnostics(lyricBox, sessBox, winBox);
}

async function refreshDiagnostics(lyricBox, sessBox, winBox) {
  try {
    const st = await invoke('lyrics_status');
    if (!st) {
      lyricBox.innerHTML =
        '<div class="st-empty">还没有发过歌词请求。播一首歌再回来看。</div>';
    } else {
      const tone = st.ok ? 'ok' : st.kind === 'notFound' ? 'warn' : 'err';
      lyricBox.innerHTML = `
        <div class="st-lyric-row">
          <span class="st-lyric-title">${esc(st.track || '—')}</span>
          ${badge(st.source || '—', 'muted')}
          ${badge('HTTP ' + (st.httpStatus || '—'), st.httpStatus === 200 ? 'ok' : 'muted')}
          ${badge(st.ok ? `${st.lineCount} 行` : kindLabel(st.kind), tone)}
          <span class="st-muted">${st.elapsedMs}ms</span>
        </div>
        <div class="st-lyric-detail">${esc(st.detail || '')}</div>`;
    }
  } catch (e) {
    lyricBox.textContent = `读取歌词状态失败：${e}`;
  }

  try {
    const sessions = await invoke('list_sessions');
    sessBox.innerHTML = (sessions || []).length
      ? sessions
          .map(
            (x) => `
        <div class="diag-row">
          <div class="diag-main">
            <div class="diag-title">${esc(x.title || '（无标题）')}</div>
            <div class="diag-sub">${esc(x.artist || '—')}</div>
            <div class="diag-meta">
              ${badge(x.playbackType || 'unknown', x.playbackType === 'video' ? 'warn' : 'ok')}
              ${x.hasTimeline ? badge('有时间轴', 'ok') : badge('无时间轴', 'warn')}
              ${x.playing ? badge('playing', 'ok') : badge('paused', 'muted')}
              ${x.isCurrent ? badge('系统当前', 'accent') : ''}
              <span class="diag-app">${esc(x.appId)}</span>
            </div>
          </div>
        </div>`
          )
          .join('')
      : '<div class="st-empty">没有任何 SMTC 会话</div>';
  } catch (e) {
    sessBox.textContent = `读取失败：${e}`;
  }

  try {
    const wins = await invoke('list_windows');
    const filtered = (wins || []).filter(
      (w) => !w.process || /kugou|qqmusic|cloudmusic|spotify|foobar|music/i.test(w.process)
    );
    winBox.innerHTML = filtered.length
      ? filtered
          .map(
            (w) => `
        <div class="diag-row diag-row--slim">
          <div class="diag-main">
            <div class="diag-title">${esc(w.title)}</div>
            <div class="diag-sub">${esc(w.process || '—')} · ${esc(w.class || '')}</div>
          </div>
        </div>`
          )
          .join('')
      : '<div class="st-empty">没找到疑似播放器的窗口</div>';
  } catch (e) {
    winBox.textContent = `读取失败：${e}`;
  }
}

function kindLabel(kind) {
  return (
    {
      notFound: '源未收录',
      networkError: '网络失败',
      parseError: '解析失败',
      rateLimited: '被限流',
      badRequest: '参数错误',
      allFailed: '全部源失败',
      ok: '成功',
    }[kind] || kind || '未知'
  );
}

function esc(s) {
  return String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
}

// ============================================================
// 入口
// ============================================================

export async function startSettingsWindow() {
  const mount = document.getElementById('settings-root');
  if (!mount) return;
  document.body.classList.add('settings-mode');

  try {
    S = await invoke('get_settings');
  } catch (e) {
    mount.innerHTML = `<div class="st-fatal">读取设置失败：${e}</div>`;
    return;
  }

  mount.innerHTML = `
    <header class="st-head">
      <h1>Lyric Island 设置</h1>
      <p class="st-sub">改动即时保存；关闭这个窗口只是隐藏，托盘里可以再打开</p>
    </header>
    <div class="st-body"></div>
    <div id="st-toast" class="st-toast"></div>`;

  const body = mount.querySelector('.st-body');
  buildGeneral(body);
  buildDesktopLyrics(body);
  buildLyrics(body);
  buildFollow(body);
  await buildDiagnostics(body);

  // Rust 侧改了设置（托盘 / 快捷键 / 另一个窗口）→ 同步刷新
  try {
    const { listen } = await import('@tauri-apps/api/event');
    await listen('settings:changed', (e) => {
      S = e.payload;
    });
    await listen('lyric:offset', () => {
      invoke('get_settings').then((s) => {
        S = s;
        renderOffsetEcho();
      });
    });
  } catch {
    /* 浏览器预览时没有事件系统 */
  }
}

function renderOffsetEcho() {
  const input = document.querySelector('.st-input--num');
  if (input) input.value = String(S?.lyricOffsetMs ?? 0);
}
