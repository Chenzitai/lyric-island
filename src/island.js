/**
 * 灵动岛渲染层。
 *
 * 职责边界：
 *  - 只管 DOM 和窗口尺寸，不知道音频、不知道歌词怎么来的；
 *  - 时间轴推进由外部每帧调用 tick() 驱动（rAF 在 main.js 里统一管）；
 *  - 窗口 resize 由这里负责，因为只有这里知道内容有多宽。
 *
 * 宽度自适应的核心是"先量再设"：
 *   用 #measure 这个脱离文档流的隐藏节点量出歌词的理想宽度，
 *   加上内边距得到岛体目标宽度，再把这个宽度同步给 Tauri 窗口。
 */

import { buildCharTimeline, findLineIndex } from './lyrics/lrc.js';

// 岛体最小/最大宽度（不含窗口留白）
const ISLAND_MIN = 132;
const ISLAND_MAX = 560;
// 窗口比岛体多出来的留白，用来放阴影和 Q 弹动画的溢出
const WINDOW_PAD_X = 12;
const WINDOW_PAD_Y = 10;

// 三种高度：收起 / 常态 / 悬停展开
const H_COLLAPSED = 26;
const H_NORMAL = 40;
const H_EXPANDED = 104;

// 展开面板自身需要的最小宽度（封面 + 歌名 + 控制 + 两个图标）
const PANEL_MIN_W = 322;

// 状态切换时的宽度过渡时长（ms），和 CSS 里的 transition 对齐
const WIDTH_ANIM_MS = 300;

// ---- 防误触手势参数 ----
// 先横向划过这么远，才算"你确实在岛体上划了一下"
const SWIPE_ARM_X = 34;
// 已横向划过后，再往下移动这么远才展开
const SWIPE_OPEN_Y = 14;
// 进入后至少停留这么久才认手势，挡住"鼠标穿屏而过"
const SWIPE_MIN_MS = 60;

function clamp(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

function fmtTime(ms) {
  const total = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

export class IslandView {
  constructor() {
    this.el = document.getElementById('island');
    this.measureEl = document.getElementById('measure');

    this.lyricAreaEl = this.el.querySelector('.lyric-area');
    this.lineTextEl = this.el.querySelector('.line-text');
    this.lineSubEl = this.el.querySelector('.line-sub');
    this.panelEl = this.el.querySelector('.panel');
    this.coverEl = this.el.querySelector('.cover-img');
    this.titleEl = this.el.querySelector('.meta-title');
    this.artistEl = this.el.querySelector('.meta-artist');
    this.progressEl = this.el.querySelector('.progress');
    this.fillEl = this.el.querySelector('.progress-fill');
    this.knobEl = this.el.querySelector('.progress-knob');
    this.remainEl = this.el.querySelector('.remain');
    this.debugEl = document.getElementById('debug-status');

    /** @type {Array<{ch:string,t0:number,t1:number}>} */
    this.chars = [];
    /** @type {HTMLElement[]} */
    this.charEls = [];

    this.expanded = false;
    this.paused = true;
    this.collapsed = false;
    this.lines = [];
    this.translations = null;
    this.currentIndex = -2;
    this.lastActiveChar = -1;
    // 当前显示的封面 data URL，用来避免重复赋值（重复赋值会让 <img> 重新解码闪一下）
    this._coverSrc = '';

    // 播放位置 / 总时长（进度条 + 剩余时间要用）
    this.positionMs = 0;
    this.durationMs = 0;
    this.hasTimeline = false;

    // 进度条拖拽状态
    this._dragging = false;
    this._dragPct = 0;

    // 设置镜像（由 main.js 同步进来）
    this.settings = {
      showTranslation: true,
      translationSize: 11,
      showProgress: true,
      collapseOnPause: false,
    };

    // 窗口尺寸缓存，避免重复调用 setSize 触发无谓的重排
    this._winW = 0;
    this._winH = 0;
    this._pendingResize = 0;

    /** @type {(w:number,h:number)=>void} 由 main.js 注入，实际调 Tauri API */
    this.resizeWindow = () => {};

    this._bindGesture();
    this._bindControls();
    this._bindSeek();
    this._applySettingsToDom();
  }

  // ---------- 对外 API ----------

  /** 注入窗口 resize 实现（浏览器里跑时是空函数） */
  setResizeHandler(fn) {
    this.resizeWindow = fn;
  }

  /** 同步设置项到 DOM（翻译开关 / 字号 / 进度条显隐 / 暂停收起） */
  applySettings(settings) {
    if (!settings) return;
    this.settings = {
      showTranslation: settings.showTranslation !== false,
      translationSize: Number(settings.translationSize) || 11,
      showProgress: settings.showProgress !== false,
      collapseOnPause: !!settings.collapseOnPause,
    };
    this._applySettingsToDom();
    this._renderLine(this.currentIndex);
    this._scheduleResize();
  }

  _applySettingsToDom() {
    const s = this.settings;
    this.el.style.setProperty('--tr-size', `${clamp(s.translationSize, 9, 20)}px`);
    this.el.dataset.showProgress = String(s.showProgress);
    this.el.dataset.showTranslation = String(s.showTranslation);
    this._updateCollapsed();
  }

  /** 换一首歌：重置高亮状态。translations 与 lines 同长度，空串表示这句没有翻译。 */
  setLyrics(lines, translations) {
    this.lines = lines || [];
    this.translations = translations || null;
    this.currentIndex = -2;
    this.lastActiveChar = -1;
    this.lineTextEl.innerHTML = '';
    this.lineSubEl.textContent = '';
    this._scheduleResize();
  }

  /** 更新播放器元信息（标题/艺术家/封面/播放态/进度） */
  setTrack({ title, artist, artwork, playing, positionMs, durationMs, hasTimeline }) {
    this.titleEl.textContent = title || '未知曲目';
    this.artistEl.textContent = artist || '';
    this.el.dataset.playing = String(!!playing);

    if (typeof durationMs === 'number') this.durationMs = durationMs;
    if (typeof positionMs === 'number') this.positionMs = positionMs;
    if (typeof hasTimeline === 'boolean') this.hasTimeline = hasTimeline;

    // 能不能拖进度条：有时长 + 播放器报的是真时间轴
    const seekable = this.hasTimeline && this.durationMs > 0;
    this.el.dataset.seekable = String(seekable);
    this.progressEl.title = seekable ? '' : '当前播放器不支持进度跳转';

    // 封面：**永远不要因为有某一帧 artwork 为空就清空**。
    //
    // SMTC 的缩略图经常晚一两帧才到，早期版本在这里 removeAttribute('src')，
    // 表现就是"封面先出现、然后闪一下没了"。现在的策略是：
    //   - 有新封面且和当前不同 → 换，并打 loaded
    //   - 新封面为空 → 什么都不做，继续显示上一张
    // 只有 setIdle()（彻底没有会话）才真正清掉。
    if (artwork && artwork !== this._coverSrc) {
      this._coverSrc = artwork;
      this.coverEl.src = artwork;
      this.coverEl.classList.add('loaded');
    }

    const wasPaused = this.paused;
    this.paused = !playing;
    this.el.dataset.paused = String(!playing);
    this._updateCollapsed();

    this._paintProgress();
    this._updateRemain();

    // 从暂停恢复时把窗口重新拉到台前（有的播放器暂停会停在某一帧）
    if (wasPaused && !this.paused && this.currentIndex === -2) {
      this._scheduleResize();
    }
  }

  /** 没有媒体会话时调用 */
  setIdle() {
    this.el.dataset.state = 'idle';
    this.el.dataset.playing = 'false';
    this.el.dataset.seekable = 'false';
    this.lineTextEl.innerHTML = '';
    this.lineSubEl.textContent = '';
    this.titleEl.textContent = '没有检测到播放器';
    this.artistEl.textContent = '在任意支持 SMTC 的应用里播放音乐';
    // 真正"没有会话"的时候才清封面
    this._coverSrc = '';
    this.coverEl.removeAttribute('src');
    this.coverEl.classList.remove('loaded');
    this.lines = [];
    this.translations = null;
    this.currentIndex = -2;
    this.durationMs = 0;
    this.positionMs = 0;
    this._paintProgress();
    this.remainEl.textContent = '-0:00';
    this._scheduleResize();
  }

  setActive() {
    this.el.dataset.state = 'active';
  }

  /**
   * 每帧推进高亮。
   * @param {number} timeMs 当前播放位置（已做插值）
   */
  tick(timeMs) {
    this.positionMs = timeMs;

    if (!this._dragging) {
      this._paintProgress();
      this._updateRemain();
    }

    if (!this.lines.length) return;
    this._lastTime = timeMs;

    const idx = findLineIndex(this.lines, timeMs);
    if (idx !== this.currentIndex) {
      this._renderLine(idx);
    }

    // 逐字推进
    if (this.chars.length) {
      let active = -1;
      for (let i = 0; i < this.chars.length; i++) {
        if (timeMs >= this.chars[i].t0) active = i;
        else break;
      }

      if (active !== this.lastActiveChar) {
        // 只改动变化的字，不要每帧全量遍历 class
        if (this.lastActiveChar >= 0) {
          const prev = this.charEls[this.lastActiveChar];
          if (prev) {
            prev.classList.remove('active');
            if (timeMs >= this.chars[this.lastActiveChar].t1) prev.classList.add('sung');
          }
        }
        // 中间跳过的字（拖动进度条）直接补上 sung
        const from = Math.min(this.lastActiveChar + 1, active);
        for (let i = Math.max(0, from); i < active; i++) {
          this.charEls[i]?.classList.add('sung');
        }
        if (active >= 0) {
          const el = this.charEls[active];
          if (el) {
            el.classList.add('active');
            if (timeMs >= this.chars[active].t1) el.classList.add('sung');
          }
        }
        this.lastActiveChar = active;
      }
    }
  }

  // ---------- 进度条 ----------

  _paintProgress() {
    const total = this.durationMs;
    let pct = 0;
    if (this._dragging) {
      pct = this._dragPct;
    } else if (total > 0) {
      pct = clamp((this.positionMs / total) * 100, 0, 100);
    }
    this.fillEl.style.width = `${pct}%`;
    this.knobEl.style.left = `${pct}%`;
  }

  _updateRemain() {
    if (this._dragging) {
      const remainMs = Math.max(0, this.durationMs * (1 - this._dragPct / 100));
      this.remainEl.textContent = `-${fmtTime(remainMs)}`;
      return;
    }
    if (this.durationMs <= 0) {
      this.remainEl.textContent = '--:--';
      return;
    }
    this.remainEl.textContent = `-${fmtTime(this.durationMs - this.positionMs)}`;
  }

  _bindSeek() {
    const bar = this.progressEl;

    const pctFromEvent = (e) => {
      const rect = bar.getBoundingClientRect();
      if (rect.width <= 0) return 0;
      return clamp(((e.clientX - rect.left) / rect.width) * 100, 0, 100);
    };

    const onDown = (e) => {
      if (this.el.dataset.seekable !== 'true') {
        // 不支持跳转：明确告诉用户，而不是默默没反应
        this.onSeekBlocked?.();
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      this._dragging = true;
      this._dragPct = pctFromEvent(e);
      // 合成事件（自动化测试）里 pointerId 不存在，捕获会抛 NotFoundError
      try {
        bar.setPointerCapture?.(e.pointerId);
      } catch {
        /* 捕获失败不影响拖动逻辑，pointermove 仍能收到 */
      }
      this.el.dataset.dragging = 'true';
      this._paintProgress();
      this._updateRemain();
    };

    const onMove = (e) => {
      if (!this._dragging) return;
      e.preventDefault();
      this._dragPct = pctFromEvent(e);
      this._paintProgress();
      this._updateRemain();
    };

    const onUp = (e) => {
      if (!this._dragging) return;
      this._dragging = false;
      this.el.dataset.dragging = 'false';
      try {
        bar.releasePointerCapture?.(e.pointerId);
      } catch {
        /* 同上 */
      }
      const target = (this.durationMs * this._dragPct) / 100;
      this.onSeek?.(target);
    };

    bar.addEventListener('pointerdown', onDown);
    bar.addEventListener('pointermove', onMove);
    bar.addEventListener('pointerup', onUp);
    bar.addEventListener('pointercancel', onUp);
  }

  // ---------- 内部 ----------

  _renderLine(idx) {
    this.currentIndex = idx;
    this.lastActiveChar = -1;
    this.chars = [];
    this.charEls = [];

    if (idx < 0) {
      // 还没唱到第一句 —— 显示"前奏"，不要假装已经唱到第一行。
      // 只有确实存在歌词、而当前进度还早于首行时间，才算前奏；
      // 没歌词的时候仍然是中性的占位符。
      const first = this.lines[0];
      const inIntro = !!first && (this._lastTime || 0) < first.time;
      this.lineTextEl.className = inIntro ? 'line-text intro' : 'line-text';
      this.lineTextEl.textContent = inIntro ? '♪ 前奏' : '· · ·';
      this._setSub('');
      this._scheduleResize();
      return;
    }

    const line = this.lines[idx];
    const next = this.lines[idx + 1];

    // 逐字渲染
    this.chars = buildCharTimeline(line);
    const frag = document.createDocumentFragment();
    for (const c of this.chars) {
      const span = document.createElement('span');
      span.className = 'ch';
      // 空格用不换行空格撑开，否则 inline span 的首尾空格会被吃掉
      span.textContent = c.ch === ' ' ? '\u00a0' : c.ch;
      frag.appendChild(span);
      this.charEls.push(span);
    }
    this.lineTextEl.className = 'line-text';
    this.lineTextEl.innerHTML = '';
    this.lineTextEl.appendChild(frag);

    // 第二行：有翻译就显示翻译，没有才退到下一句预览。
    // 关掉"显示翻译"时，连下一句预览也不显示 —— 用户要的就是单行。
    const tr = this.translations ? this.translations[idx] : '';
    if (this.settings.showTranslation) {
      this._setSub((tr && tr.trim()) || (next ? next.text : ''));
    } else {
      this._setSub('');
    }

    this._scheduleResize();
  }

  _setSub(text) {
    if (text) {
      this.lineSubEl.textContent = text;
      this.lineSubEl.hidden = false;
    } else {
      this.lineSubEl.textContent = '';
      this.lineSubEl.hidden = true;
    }
  }

  _updateCollapsed() {
    // 暂停即收起 —— 哪怕面板正展开着。这个选项的语义就是"少占地方"，
    // 暂停时还挂着大面板反而更碍事。
    const shouldCollapse = this.settings.collapseOnPause && this.paused;
    if (shouldCollapse && this.expanded) {
      this.setExpanded(false);
    }
    if (this.collapsed === shouldCollapse) return;
    this.collapsed = shouldCollapse;
    this.el.dataset.collapsed = String(shouldCollapse);
    this._scheduleResize();
  }

  /** 计算岛体应该多宽 */
  _measureIslandWidth() {
    const text = this.lineTextEl.textContent || '';
    const sub = this.settings.showTranslation ? this.lineSubEl.textContent || '' : '';

    // 用同一套字体量，避免量出来的宽度和渲染出来的不一致
    this.measureEl.style.fontSize = this.expanded ? '14px' : '15px';
    this.measureEl.style.fontWeight = '600';
    this.measureEl.textContent = text || '· · ·';
    const w1 = this.measureEl.offsetWidth;

    this.measureEl.style.fontSize = `${this.settings.translationSize}px`;
    this.measureEl.style.fontWeight = '400';
    this.measureEl.textContent = sub;
    const w2 = this.measureEl.offsetWidth;

    // 左右 padding 18*2 = 36，再留 6px 呼吸
    const lyricW = Math.max(w1, Math.min(w2, 420)) + 36 + 6;

    if (!this.expanded) return clamp(lyricW, ISLAND_MIN, ISLAND_MAX);

    // 展开态：歌词在上一行，下面是面板，取两者较宽的那个
    return clamp(Math.max(lyricW, PANEL_MIN_W + 28), ISLAND_MIN, ISLAND_MAX);
  }

  _scheduleResize() {
    if (this._pendingResize) return;
    // 用 setTimeout 而不是 requestAnimationFrame：
    // 窗口在后台/被遮挡时 rAF 会被浏览器节流到 1fps 甚至暂停，
    // 那样尺寸变化要等半秒才生效，非常明显。
    this._pendingResize = setTimeout(() => {
      this._pendingResize = 0;
      this._applyResize();
    }, 16);
  }

  _applyResize() {
    const islandW = this._measureIslandWidth();
    const islandH = this.expanded ? H_EXPANDED : this.collapsed ? H_COLLAPSED : H_NORMAL;

    const w = Math.ceil(islandW + WINDOW_PAD_X);
    const h = Math.ceil(islandH + WINDOW_PAD_Y);

    if (w === this._winW && h === this._winH) return;
    this._winW = w;
    this._winH = h;

    this.resizeWindow(w, h);
  }

  // ---------- 防误触手势 ----------
  //
  // 直接用鼠标移入就展开，最大的问题是"从屏幕上方穿过去拿窗口"时被弹面板挡住。
  // 原作者的解法是要求一个特定手势：先横向划过岛体、再往下带一下，才展开。
  //
  // 判定分两段：
  //   1) 横向位移累计 ≥ SWIPE_ARM_X  → 进入 armed 状态（CSS 会亮一条提示线）
  //   2) armed 状态下纵向位移 ≥ SWIPE_OPEN_Y（向下）→ 展开
  //
  // 鼠标离开岛体、或者已经展开过，就把状态清掉。
  _bindGesture() {
    let entering = false;
    let enterAt = 0;
    let originX = 0;
    let originY = 0;
    let armed = false;
    let closeTimer = 0;

    const reset = () => {
      entering = false;
      armed = false;
      this.el.dataset.armed = 'false';
      this.el.dataset.hovering = 'false';
    };

    this.el.addEventListener('pointerenter', (e) => {
      clearTimeout(closeTimer);
      if (this.expanded) return;
      entering = true;
      armed = false;
      enterAt = performance.now();
      originX = e.clientX;
      originY = e.clientY;
      this.el.dataset.hovering = 'true';
      this.el.dataset.armed = 'false';
    });

    this.el.addEventListener('pointermove', (e) => {
      if (!entering || this.expanded) return;

      const dx = Math.abs(e.clientX - originX);
      const dy = e.clientY - originY;

      if (!armed) {
        if (dx >= SWIPE_ARM_X && performance.now() - enterAt >= SWIPE_MIN_MS) {
          armed = true;
          this.el.dataset.armed = 'true';
        }
        return;
      }

      // 已经横向划过，现在往下带一下就展开
      if (dy >= SWIPE_OPEN_Y) {
        armed = false;
        entering = false;
        this.el.dataset.armed = 'false';
        this.el.dataset.hovering = 'false';
        this.setExpanded(true);
      }
    });

    this.el.addEventListener('pointerleave', () => {
      reset();
      if (!this.expanded) return;
      clearTimeout(closeTimer);
      closeTimer = window.setTimeout(() => this.setExpanded(false), 260);
    });

    // 展开后鼠标再离开一次（此时岛体已经变大，指针还是在里面），
    // 所以额外监听面板内部的离开
    this.el.addEventListener('pointercancel', reset);
  }

  setExpanded(v) {
    if (this.expanded === v) return;
    this.expanded = v;
    this.el.dataset.expanded = String(v);
    this.el.dataset.hovering = 'false';
    this.el.dataset.armed = 'false';
    if (v) this.el.dataset.collapsed = 'false';
    // 展开动画过程中多次测量会导致窗口抖动，等 CSS 过渡走完再定最终尺寸
    setTimeout(() => this._scheduleResize(), WIDTH_ANIM_MS);
    this._scheduleResize();
    this.onExpandedChange?.(v);
  }

  _bindControls() {
    this.el.querySelectorAll('.ctl').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        const action = btn.dataset.action;
        this.onControl?.(action);
      });
    });

    this.el.querySelectorAll('.icon-btn').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        const action = btn.dataset.action;
        if (action === 'settings') this.onOpenSettings?.();
        else if (action === 'playlist') this.onOpenPlaylist?.();
      });
    });
  }

  setDebugText(text) {
    if (!this.debugEl) return;
    this.debugEl.textContent = text;
    this.debugEl.classList.toggle('visible', !!text);
  }
}
