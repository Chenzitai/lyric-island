/**
 * LRC 解析器 + 逐字时间轴生成。
 *
 * 为什么逐字要"生成"：LRCLIB 大部分条目只有行级时间戳（`[mm:ss.xx]`），
 * 没有 QQ 音乐 QRC / 网易云 YRC 那种每个字的时间。业内通行做法（Lyricify、
 * Lyricod 的降级路径）是按字符数在当前行的时间区间内均分，标点处留一点停顿，
 * 视觉上够用，也不至于每行都匀速得像机器人。
 *
 * 关键点：行的时间区间不能用 `nextLine.time - line.time`。
 * 一句歌词唱完后往往有 2~6 秒的间奏，按那个区间均分会导致每个字"爬"得非常慢，
 * 高亮永远追不上。所以用 max(实测所需时长, 下限) 截断 —— 见 estimateLineEnd()。
 */

const TIME_TAG = /\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]/g;
const META_TAG = /^\[(ti|ar|al|by|offset|length|re|ve):(.*)\]$/i;

// 一个字平均"唱多久"（毫秒）。中文说唱快的时候 120ms/字，慢歌 260ms/字。
const MS_PER_CHAR_MIN = 110;
const MS_PER_CHAR_MAX = 260;
// 一行唱完后的最小/最大渲染时长
const LINE_DUR_MIN = 900;
const LINE_DUR_MAX = 12000;

/**
 * 解析 LRC 文本
 * @param {string} raw
 * @returns {{ meta: object, lines: Array<{time:number, text:string, end:number}> }}
 */
export function parseLrc(raw) {
  const meta = {};
  /** @type {Map<number, string>} 时间 -> 文本（同一时间多个标签只保留一个） */
  const byTime = new Map();

  if (!raw || typeof raw !== 'string') {
    return { meta, lines: [] };
  }

  // 统一换行，去掉 BOM
  const text = raw.replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n');

  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim();
    if (!line) continue;

    // 元数据标签
    const metaMatch = line.match(META_TAG);
    if (metaMatch) {
      const key = metaMatch[1].toLowerCase();
      const value = metaMatch[2].trim();
      meta[key] = key === 'offset' ? Number(value) || 0 : value;
      continue;
    }

    // 收集这一行上所有的 [mm:ss.xx] 标签（一行可以挂多个时间戳）
    const stamps = [];
    TIME_TAG.lastIndex = 0;
    let m;
    let lastIndex = 0;
    while ((m = TIME_TAG.exec(line)) !== null) {
      // 标签必须紧贴行首，否则像 "[这是歌词]" 会被误判
      if (m.index !== lastIndex) break;
      lastIndex = m.index + m[0].length;

      const min = parseInt(m[1], 10);
      const sec = parseInt(m[2], 10);
      let frac = 0;
      if (m[3] !== undefined) {
        // .5 -> 500ms, .55 -> 550ms, .555 -> 555ms
        frac = parseInt(m[3].padEnd(3, '0'), 10);
      }
      stamps.push(min * 60000 + sec * 1000 + frac);
    }

    if (stamps.length === 0) continue;

    const content = line.slice(lastIndex).trim();
    if (!content) continue;

    for (const t of stamps) {
      if (!byTime.has(t)) byTime.set(t, content);
    }
  }

  const times = [...byTime.keys()].sort((a, b) => a - b);
  const offset = Number(meta.offset) || 0;

  let lines = times.map((t, i) => {
    const next = i + 1 < times.length ? times[i + 1] : t + LINE_DUR_MAX;
    const text = byTime.get(t);
    return {
      time: Math.max(0, t - offset),
      end: estimateLineEnd(text, t, next) - offset,
      text,
    };
  });

  // 去掉开头的制作人员名单。
  // 很多 LRC 的第一行是「作词 : xxx」「Composed by xxx」，它不是歌词，
  // 却会因为时间戳是 00:00 而被当成第一句点亮 —— 观感上像"歌词串了"。
  // 只在前面 15 秒内过滤，且过滤后不能变空（万一整首就一行）。
  if (lines.length > 1) {
    const kept = lines.filter((l) => !(l.time < 15000 && CREDIT_LINE.test(l.text)));
    if (kept.length) lines = kept;
  }

  return { meta, lines };
}

/** 制作名单行的特征：以「作词/作曲/…」或英文 credit 开头，后面跟冒号 */
const CREDIT_LINE =
  /^(作词|作曲|编曲|制作人|监制|出品人?|词|曲|混音|母带|录音|和声|吉他|贝斯|键盘|弦乐|program|Lyrics?|Composed|Arranged|Produced|Written|Mixed|Mastered|Performed)\s*[:：]/i;

/**
 * 估算一行的结束时间。
 *
 * 思路：
 *  1. 先按字数算一个"合理演唱时长"（中文按字，英文按词折算）；
 *  2. 如果下一行离得比这个时长远，说明中间有间奏 —— 用合理时长收尾，
 *     不要拉到下一行，否则高亮会拖尾；
 *  3. 如果下一行离得很近（连唱），就用下一行的时间。
 */
function estimateLineEnd(text, start, nextStart) {
  const len = visualLength(text);
  const natural = clamp(len * 190, LINE_DUR_MIN, LINE_DUR_MAX);
  const gap = nextStart - start;

  // 连唱：下一行出现得比"自然时长"还早，直接贴近下一行
  if (gap <= natural * 1.05) {
    return Math.max(start + LINE_DUR_MIN * 0.6, nextStart - 90);
  }

  // 有间奏：按自然时长收尾，但至少给足一个字的时间
  return start + Math.max(LINE_DUR_MIN, natural);
}

/** 视觉长度：中日韩字符算 1，ASCII 每 2 个算 1（英文按音节粗略折算） */
function visualLength(text) {
  let n = 0;
  for (const ch of text) {
    if (/[\u4e00-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(ch)) n += 1;
    else if (/[a-zA-Z0-9]/.test(ch)) n += 0.5;
    else if (/\s/.test(ch)) n += 0.15;
    else n += 0.6; // 标点
  }
  return Math.max(1, n);
}

/**
 * 把一行拆成逐字时间轴。
 *
 * @param {{time:number, end:number, text:string}} line
 * @returns {Array<{ch:string, t0:number, t1:number}>}
 */
export function buildCharTimeline(line) {
  const chars = [...line.text];
  if (chars.length === 0) return [];

  const start = line.time;
  const end = Math.max(line.end, start + 300);
  const span = end - start;

  // 每个字的权重：中文 1，英文 0.55，标点 0.7 但后面额外加一个"停顿权重"
  const weights = chars.map((ch) => {
    if (/\s/.test(ch)) return 0.2;
    if (/[\u4e00-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(ch)) return 1;
    if (/[a-zA-Z0-9]/.test(ch)) return 0.55;
    return 0.7;
  });

  // 标点后加停顿：让"，"和"。"之后的高亮自然停一下
  const pauses = chars.map((ch) => (/[，。！？、,.!?;；:：…—]/.test(ch) ? 0.55 : 0));
  const totalWeight = weights.reduce((a, b) => a + b, 0) + pauses.reduce((a, b) => a + b, 0);

  const unit = span / Math.max(totalWeight, 0.001);

  const out = [];
  let cursor = start;
  for (let i = 0; i < chars.length; i++) {
    const dur = weights[i] * unit;
    out.push({ ch: chars[i], t0: cursor, t1: cursor + dur });
    cursor += dur + pauses[i] * unit;
  }

  return out;
}

function clamp(v, min, max) {
  return Math.min(max, Math.max(min, v));
}

/**
 * 在已排序的行数组里，找出当前时间对应的行下标。
 * 返回 -1 表示还没到第一行。
 */
export function findLineIndex(lines, timeMs) {
  if (!lines.length) return -1;
  if (timeMs < lines[0].time) return -1;

  // 二分：歌词动辄几百行、每帧都要查，线性扫太浪费
  let lo = 0;
  let hi = lines.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (lines[mid].time <= timeMs) {
      ans = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return ans;
}
