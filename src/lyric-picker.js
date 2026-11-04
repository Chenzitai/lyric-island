/**
 * 「选择歌词」窗口。
 *
 * 为什么需要它：
 *   自动匹配再怎么调，也一定会有"源里根本没有"和"源里有但匹配错了"两种情况。
 *   前者需要换源，后者需要人工指认。这个窗口把选择权交还给用户。
 *
 * 选完之后写的是 **永久绑定**（lyric-bindings.json），
 * 以后每次播这首歌都直接走绑定的那条，不再重新搜 —— 清缓存也不丢。
 */

import { invoke } from '@tauri-apps/api/core';

const SOURCE_LABEL = {
  netease: '网易云',
  qq: 'QQ 音乐',
  lrclib: 'LRCLIB',
};

function fmtDuration(sec) {
  if (!sec || !isFinite(sec)) return '';
  const m = Math.floor(sec / 60);
  const s = Math.round(sec % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text != null) node.textContent = text;
  return node;
}

export async function startLyricPicker() {
  const root = document.getElementById('picker-root');
  if (!root) return;

  document.body.dataset.window = 'picker';

  // ---- 骨架 ----
  const wrap = el('div', 'pk');

  const head = el('div', 'pk-head');
  head.append(el('div', 'pk-title', '选择歌词'));
  head.append(el('div', 'pk-sub', '选错的歌词会一直错下去，点一行指定正确的版本'));
  wrap.append(head);

  const query = el('div', 'pk-query');
  const inTitle = el('input', 'pk-in pk-in-title');
  inTitle.placeholder = '歌曲名';
  const inArtist = el('input', 'pk-in pk-in-artist');
  inArtist.placeholder = '歌手';
  const btnSearch = el('button', 'pk-btn', '搜索');
  query.append(inTitle, inArtist, btnSearch);
  wrap.append(query);

  const status = el('div', 'pk-status', '');
  wrap.append(status);

  const list = el('div', 'pk-list');
  wrap.append(list);

  root.append(wrap);

  // ---- 状态 ----
  let candidates = [];
  let currentTrack = { title: '', artist: '', durationMs: 0 };
  let busy = false;

  function setStatus(text, kind) {
    status.textContent = text || '';
    status.dataset.kind = kind || '';
  }

  async function loadCurrentTrack() {
    try {
      const np = await invoke('now_playing');
      if (np && np.active) {
        currentTrack = {
          title: np.title || '',
          artist: np.artist || '',
          durationMs: np.durationMs || 0,
        };
        inTitle.value = currentTrack.title || '';
        inArtist.value = currentTrack.artist || '';
        return;
      }
    } catch {
      /* 没在播放就留空，让用户手打 */
    }
    setStatus('当前没有正在播放的曲目，可以手动输入歌名搜索', 'warn');
  }

  function renderList() {
    list.innerHTML = '';
    if (!candidates.length) return;

    candidates.forEach((cand, i) => {
      const row = el('div', 'pk-item');
      row.dataset.index = String(i);

      row.append(el('div', 'pk-src', SOURCE_LABEL[cand.source] || cand.source));

      const main = el('div', 'pk-main');
      const name = el('div', 'pk-name', cand.title || '(无标题)');
      // 得分最高的那条给个"最佳"标记，省得用户自己比分数
      if (i === 0) name.append(el('span', 'pk-best', '最佳匹配'));
      main.append(name);
      main.append(
        el(
          'div',
          'pk-meta',
          [cand.artist || '未知歌手', fmtDuration(cand.duration), cand.album || '']
            .filter(Boolean)
            .join(' · '),
        ),
      );
      if (cand.preview) main.append(el('div', 'pk-preview', cand.preview));
      row.append(main);

      const badges = el('div', 'pk-badges');
      if (cand.hasWordSync) badges.append(el('span', 'pk-badge pk-badge-word', '逐字'));
      else if (cand.hasSynced) badges.append(el('span', 'pk-badge', '逐行'));
      if (cand.hasTranslation) badges.append(el('span', 'pk-badge pk-badge-tr', '翻译'));
      row.append(badges);

      row.append(el('div', 'pk-score', String(cand.score ?? '')));

      row.addEventListener('click', () => pick(cand));
      list.append(row);
    });
  }

  async function search() {
    if (busy) return;
    const title = inTitle.value.trim();
    if (!title) {
      setStatus('先填个歌名', 'warn');
      return;
    }

    busy = true;
    btnSearch.disabled = true;
    setStatus('正在搜索各源…', '');
    list.innerHTML = '';

    try {
      candidates = await invoke('search_lyrics_candidates', {
        title,
        artist: inArtist.value.trim(),
        album: null,
        // 手动搜索时不带时长：用户可能就是嫌弃自动匹配的时长对不上
        duration: null,
      });
      if (!candidates.length) {
        setStatus('各源都没有找到，换个关键词试试（比如只留歌名）', 'warn');
      } else {
        setStatus(`找到 ${candidates.length} 条候选，点一行即绑定`, 'ok');
      }
      renderList();
    } catch (err) {
      setStatus(`搜索失败：${err}`, 'err');
    } finally {
      busy = false;
      btnSearch.disabled = false;
    }
  }

  async function pick(cand) {
    if (busy) return;
    busy = true;
    setStatus(`正在应用「${cand.title}」…`, '');

    try {
      // 绑定存在"这首歌对应的 key"上，key 由 Rust 侧的 normalize 规则算，
      // 前端不要自己拼，否则规则一变绑定就全失效了。
      await invoke('set_lyric_binding', {
        title: currentTrack.title || inTitle.value.trim(),
        artist: currentTrack.artist || inArtist.value.trim(),
        candidate: cand,
        offsetMs: 0,
      });

      // 让岛体丢掉缓存重新拉一次（会命中刚写的绑定）
      const { emit } = await import('@tauri-apps/api/event');
      await emit('lyric:rebind', { source: cand.source, sourceId: cand.sourceId });

      setStatus('已绑定，正在关闭…', 'ok');
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      setTimeout(() => getCurrentWindow().close(), 300);
    } catch (err) {
      setStatus(`绑定失败：${err}`, 'err');
      busy = false;
    }
  }

  btnSearch.addEventListener('click', search);
  inTitle.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') search();
  });
  inArtist.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') search();
  });

  await loadCurrentTrack();
  // 有当前曲目就自动搜一次，省掉一次点击
  if (inTitle.value.trim()) await search();
  else setStatus('输入歌名后回车或点搜索', '');
}
