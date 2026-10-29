// matinee portal. Plain ES module, no dependencies. All data is inserted with textContent.

const $ = (id) => document.getElementById(id);
const state = { folder: '', q: '', videos: [], current: null };
const SAVE_EVERY_MS = 10000;

async function api(path) {
  const res = await fetch(path);
  if (!res.ok) throw new Error(path + ' -> ' + res.status);
  return res.json();
}

function putJson(path, body, keepalive) {
  return fetch(path, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
    keepalive: !!keepalive,
  }).then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))));
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function fmtDuration(s) {
  if (s == null) return '';
  s = Math.round(s);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

// ---------------------------------------------------------------- grid

function cardFor(v) {
  const card = el('button', 'card');
  card.type = 'button';
  card.dataset.id = String(v.id);
  const img = el('img', 'thumb');
  img.loading = 'lazy';
  img.alt = '';
  img.src = `/thumb/${v.id}`;
  card.append(img);
  if (v.quest_playable === false) card.append(el('span', 'badge', 'May not play on Quest'));
  if (v.watched) card.append(el('span', 'check', '✓'));
  const body = el('div', 'card-body');
  body.append(el('div', 'card-title', v.title));
  const meta = [fmtDuration(v.duration), v.folder].filter(Boolean).join(' · ');
  body.append(el('div', 'card-meta', meta || ' '));
  // Always render the bar track so every card has the same height.
  const bar = el('div', 'prog');
  if (v.duration && v.position > 0 && !v.watched) {
    const fill = document.createElement('i');
    fill.style.width = Math.min(100, (v.position / v.duration) * 100) + '%';
    bar.append(fill);
  }
  body.append(bar);
  card.append(body);
  card.addEventListener('click', () => openPlayer(v, card));
  return card;
}

function renderGrid() {
  $('grid').replaceChildren(...state.videos.map(cardFor));
  $('empty').hidden = state.videos.length > 0;
}

async function loadVideos() {
  const params = new URLSearchParams();
  if (state.q) params.set('q', state.q);
  if (state.folder) params.set('folder', state.folder);
  try {
    state.videos = await api('/api/videos?' + params);
  } catch (e) {
    console.error(e);
    state.videos = [];
  }
  renderGrid();
}

// ---------------------------------------------------------------- folders

const expanded = new Set(['']);
let treeData = null;

function flatten(node, depth, out) {
  out.push({ node, depth });
  node.children.forEach((c) => flatten(c, depth + 1, out));
  return out;
}

function renderTreeNode(node) {
  const li = el('li');
  const row = el('div', 'tree-row');
  const hasKids = node.children.length > 0;
  const open = expanded.has(node.path);
  const exp = el('button', 'tree-exp', hasKids ? (open ? '▾' : '▸') : '');
  exp.type = 'button';
  exp.disabled = !hasKids;
  if (hasKids) exp.setAttribute('aria-label', 'Expand or collapse ' + node.name);
  exp.addEventListener('click', () => {
    if (open) expanded.delete(node.path); else expanded.add(node.path);
    renderTree();
  });
  const sel = el('button', 'tree-sel' + (state.folder === node.path ? ' active' : ''));
  sel.type = 'button';
  sel.append(el('span', '', node.path === '' ? 'All videos' : node.name), el('span', 'tree-count', String(node.count)));
  sel.addEventListener('click', () => selectFolder(node.path));
  row.append(exp, sel);
  li.append(row);
  if (hasKids && open) {
    const ul = el('ul');
    node.children.forEach((c) => ul.append(renderTreeNode(c)));
    li.append(ul);
  }
  return li;
}

function renderTree() {
  if (!treeData) return;
  const ul = el('ul');
  ul.append(renderTreeNode(treeData));
  $('tree').replaceChildren(ul);
  const select = $('folder-select');
  select.replaceChildren(
    ...flatten(treeData, 0, []).map(({ node, depth }) => {
      const name = node.path === '' ? 'All videos' : node.name;
      const o = el('option', '', '  '.repeat(depth) + name + ' (' + node.count + ')');
      o.value = node.path;
      return o;
    })
  );
  select.value = state.folder;
}

async function loadFolders() {
  try {
    treeData = await api('/api/folders');
  } catch (e) {
    console.error(e);
    return;
  }
  renderTree();
}

function selectFolder(path) {
  state.folder = path;
  renderTree();
  loadVideos();
}

// ---------------------------------------------------------------- player

const video = $('video');
let saveTimer = null;
let originRect = null;

function rectClip(r) {
  const w = window.innerWidth;
  const h = window.innerHeight;
  return `inset(${r.top}px ${w - r.right}px ${h - r.bottom}px ${r.left}px round 12px)`;
}

function updateWatchButton() {
  $('watch-toggle').textContent = state.current && state.current.watched ? 'Mark unwatched' : 'Mark watched';
}

function saveProgress(keepalive) {
  const cur = state.current;
  if (!cur) return Promise.resolve();
  const pos = video.currentTime;
  if (!Number.isFinite(pos) || pos < 0) return Promise.resolve();
  return putJson(`/api/videos/${cur.id}/progress`, { position: pos }, keepalive)
    .then((v) => {
      if (state.current && state.current.id === v.id) {
        state.current = v;
        updateWatchButton();
      }
    })
    .catch((e) => console.warn('progress save failed', e));
}

function startTimer() {
  stopTimer();
  saveTimer = setInterval(() => saveProgress(false), SAVE_EVERY_MS);
}

function stopTimer() {
  if (saveTimer) clearInterval(saveTimer);
  saveTimer = null;
}

function applySubs() {
  const idx = Number($('subs-select').value);
  Array.from(video.textTracks).forEach((t, i) => {
    t.mode = i === idx ? 'showing' : 'disabled';
  });
}

function subLabel(s) {
  if (!s.lang) return s.label;
  const code = s.lang.toLowerCase();
  try {
    const name = new Intl.DisplayNames([navigator.language], { type: 'language' }).of(code);
    return name && name !== code ? name : code;
  } catch (e) {
    return code;
  }
}

function showVideoError(v) {
  let sub;
  if (v.quest_playable === false) sub = "Codec: " + v.codec + " isn't supported by the Quest browser.";
  else if (v.codec == null) sub = 'The file may be incomplete or not a valid MP4.';
  else sub = 'Try opening it in DeoVR instead.';
  $('video-error-main').textContent = "This video can't be played here.";
  $('video-error-sub').textContent = sub;
  $('video-error').hidden = false;
}

async function openPlayer(v, card) {
  state.current = v;
  $('video-error').hidden = true;
  originRect = card ? card.getBoundingClientRect() : null;
  $('player-title').textContent = v.title;
  updateWatchButton();
  video.replaceChildren();
  const select = $('subs-select');
  const off = el('option', '', 'Off');
  off.value = '-1';
  select.replaceChildren(off);
  select.parentElement.hidden = !v.has_subs;

  video.preload = 'metadata';
  video.src = `/stream/${v.id}`;
  video.addEventListener(
    'loadedmetadata',
    () => {
      if (v.position > 0 && !v.watched && Number.isFinite(video.duration) && v.position < video.duration) {
        video.currentTime = v.position;
      }
    },
    { once: true }
  );

  const player = $('player');
  player.hidden = false;
  document.body.style.overflow = 'hidden';
  if (originRect && player.animate) {
    player.animate(
      [{ clipPath: rectClip(originRect) }, { clipPath: 'inset(0px 0px 0px 0px round 0px)' }],
      { duration: 220, easing: 'ease-out' }
    );
  }
  $('close').focus();

  if (v.has_subs) {
    try {
      const subs = await api(`/api/videos/${v.id}/subs`);
      if (state.current !== v) return;
      subs.forEach((s) => {
        const t = document.createElement('track');
        t.kind = 'subtitles';
        const label = subLabel(s);
        t.label = label;
        if (s.lang) t.srclang = s.lang.toLowerCase();
        t.src = s.url;
        video.append(t);
        const o = el('option', '', label);
        o.value = String(s.index);
        select.append(o);
      });
      applySubs(); // all tracks start disabled until the user picks one
    } catch (e) {
      console.warn('subs failed', e);
    }
  }
}

async function closePlayer() {
  const player = $('player');
  if (player.hidden || !state.current) return;
  stopTimer();
  const id = state.current.id;
  const rect = originRect;
  video.pause();
  await saveProgress(true);
  state.current = null;
  const finish = () => {
    player.hidden = true;
    video.removeAttribute('src');
    video.replaceChildren();
    video.load();
    document.body.style.overflow = '';
  };
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (rect && player.animate && !reduced) {
    const a = player.animate(
      [{ clipPath: 'inset(0px 0px 0px 0px round 0px)' }, { clipPath: rectClip(rect) }],
      { duration: 180, easing: 'ease-in' }
    );
    a.onfinish = finish;
    a.oncancel = finish;
  } else {
    finish();
  }
  refreshCard(id);
}

async function refreshCard(id) {
  try {
    const fresh = await api(`/api/videos/${id}`);
    const i = state.videos.findIndex((x) => x.id === id);
    if (i >= 0) {
      state.videos[i] = fresh;
      const old = $('grid').querySelector(`[data-id="${id}"]`);
      if (old) old.replaceWith(cardFor(fresh));
    }
  } catch (e) {
    console.warn(e);
  }
}

async function toggleWatched() {
  const cur = state.current;
  if (!cur) return;
  try {
    state.current = await putJson(`/api/videos/${cur.id}/watched`, { watched: !cur.watched });
    updateWatchButton();
  } catch (e) {
    console.warn(e);
  }
}

// ---------------------------------------------------------------- status / rescan

let polling = false;

async function pollStatus() {
  let st;
  try {
    st = await api('/api/status');
  } catch (e) {
    return;
  }
  const btn = $('rescan');
  btn.disabled = st.scanning;
  btn.textContent = st.scanning ? 'Scanning…' : 'Rescan';
  if (st.scanning) {
    polling = true;
    setTimeout(pollStatus, 1000);
  } else if (polling) {
    polling = false;
    await Promise.all([loadFolders(), loadVideos()]);
  }
}

async function rescan() {
  const btn = $('rescan');
  btn.disabled = true;
  btn.textContent = 'Scanning…';
  polling = true;
  try {
    await fetch('/api/rescan', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    });
  } catch (e) {
    console.warn(e);
  }
  setTimeout(pollStatus, 500);
}

// ---------------------------------------------------------------- init

function debounce(fn, ms) {
  let t;
  return (...a) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
}

function init() {
  $('addr').textContent = 'DeoVR: http://' + location.host;
  $('search').addEventListener(
    'input',
    debounce((e) => {
      state.q = e.target.value.trim();
      loadVideos();
    }, 250)
  );
  $('rescan').addEventListener('click', rescan);
  $('folder-select').addEventListener('change', (e) => selectFolder(e.target.value));
  $('tree-toggle').addEventListener('click', () => {
    const hide = !$('tree').classList.contains('collapsed');
    $('tree').classList.toggle('collapsed', hide);
    document.querySelector('.layout').classList.toggle('tree-hidden', hide);
    $('tree-toggle').setAttribute('aria-expanded', String(!hide));
  });
  $('close').addEventListener('click', closePlayer);
  $('watch-toggle').addEventListener('click', toggleWatched);
  $('subs-select').addEventListener('change', applySubs);
  video.addEventListener('error', () => {
    // Ignore the error raised by clearing src when the player closes.
    if (state.current && video.getAttribute('src')) showVideoError(state.current);
  });
  video.addEventListener('play', startTimer);
  video.addEventListener('pause', () => {
    stopTimer();
    saveProgress(false);
  });
  video.addEventListener('ended', () => {
    stopTimer();
    saveProgress(false);
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closePlayer();
  });
  window.addEventListener('pagehide', () => saveProgress(true));
  loadFolders();
  loadVideos();
  pollStatus();
}

init();
