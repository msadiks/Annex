// The Annex window: Displays, Connection and Preferences, plus Pair with a PC.
'use strict';

const PAGES = {
  displays: ['Displays', 'Turn a PC monitor on and your Mac sees a new display. Turn it off and it’s unplugged.'],
  connection: ['Connection', 'Annex works only on your own network. Nothing leaves it.'],
  prefs: ['Preferences', 'How Annex starts, and what it’s allowed to do on this Mac.'],
};
const PAGE_ORDER = ['displays', 'connection', 'prefs'];
const SHARP = '#F5F5F5';
const FAST = '#8C8C8C';
const HISTORY = 28;

const ui = {
  page: 'displays',
  shownPage: null,
  selected: null,
  shownSelected: null,
  /** The pairing view, opened by hand while already paired. */
  pairing: false,
  /** none | sent | failed: a pairing started from this window. */
  attempt: 'none',
  sentAt: 0,
  sawWorking: false,
  showDone: false,
  pc: null,
  pin: '',
  pinFocused: false,
  pinMsg: '',
  forgetAsk: false,
  error: '',
};
let current = null;
const hist = { total: [], mon: new Map() };

window.annexGo = (page) => go(page);

function go(page) {
  if (page === 'pair') {
    ui.pairing = true;
    ui.showDone = false;
  } else if (PAGES[page]) {
    ui.pairing = false;
    ui.page = page;
    ui.forgetAsk = false;
  }
  if (current) render(current);
}

// ---------- history ----------

function push(a, x) {
  a.push(x);
  if (a.length > HISTORY) a.shift();
}

function record(v) {
  push(hist.total, totals(v).mbps ?? 0);
  const seen = new Set();
  for (const s of v.streams) {
    seen.add(s.monitor_id);
    let m = hist.mon.get(s.monitor_id);
    if (!m) hist.mon.set(s.monitor_id, (m = { fps: [], mbps: [], ms: [], loss: [] }));
    if (!s.live) continue;
    push(m.fps, s.fps_presented);
    push(m.mbps, s.bitrate_mbps);
    push(m.ms, s.e2e_ms ?? 0);
    push(m.loss, s.loss_percent);
  }
  for (const id of [...hist.mon.keys()]) if (!seen.has(id)) hist.mon.delete(id);
}

/** Fills `el` with HISTORY bars, newest on the right. `norm` maps a value to 0–100. */
function bars(el, values, norm, color, live) {
  while (el.children.length < HISTORY) el.append(h('span'));
  const pad = HISTORY - values.length;
  [...el.children].forEach((b, i) => {
    const x = values[i - pad];
    const pct = live && x != null ? Math.max(6, Math.min(100, norm(x))) : 6;
    setStyle(b, 'height', `${pct.toFixed(1)}%`);
    setStyle(b, 'opacity', (0.18 + (0.82 * i) / (HISTORY - 1)).toFixed(2));
    setStyle(b, 'background-color', live ? color : '#2E2E2E');
  });
}
const peak = (a, floor) => Math.max(floor, ...a);

// ---------- shared bits ----------

function setDot(el, v) {
  const c = v.state === 'connected' ? 'dot live' : v.state === 'not_paired' ? 'dot none' : 'dot wait';
  if (el.className !== c) el.className = c;
}

function stateLine(v) {
  const pc = v.paired_name || 'the PC';
  const live = totals(v).live;
  if (v.state === 'connected') return live ? `Streaming ${plural(live, 'display')}` : 'Connected';
  if (v.state === 'connecting') return `Connecting to ${pc}…`;
  if (v.state === 'searching') {
    return v.grace_secs != null && v.kept_displays
      ? `Keeping ${plural(v.kept_displays, 'display')} for ${v.grace_secs} s`
      : `Looking for ${pc}…`;
  }
  return 'Not paired';
}

function streamOf(v, id) {
  return v.streams.find((s) => s.monitor_id === id);
}

/** Sharp or Fast: what the monitor streams now, or what it will when on. */
function effMode(m, s) {
  if (s) return s.mode === 'Sharp' ? 'sharp' : 'fast';
  return m.mode === 'sharp' ? 'sharp' : 'fast';
}

function setSwitch(el, on) {
  setAttr(el, 'aria-checked', on ? 'true' : 'false');
}

function setSeg(el, value) {
  const buttons = [...el.querySelectorAll('button')];
  const i = Math.max(0, buttons.findIndex((b) => b.dataset.v === value));
  setStyle(el, '--i', i);
  buttons.forEach((b, j) => setAttr(b, 'aria-pressed', j === i ? 'true' : 'false'));
}

// ---------- render ----------

/** The engine's pairing state, once it is about the attempt made here and not an older one. */
function attemptResult(v) {
  if (ui.attempt !== 'sent') return null;
  if (v.pairing === 'working') ui.sawWorking = true;
  const fresh = ui.sawWorking || Date.now() - ui.sentAt > 1200;
  return fresh && (v.pairing === 'done' || v.pairing === 'failed') ? v.pairing : null;
}

function render(v) {
  current = v;
  const paired = v.state !== 'not_paired';
  const result = attemptResult(v);
  if (result === 'failed') {
    ui.attempt = 'failed';
    replay($('cells'), 'shake');
  }
  if (result === 'done') {
    ui.attempt = 'none';
    ui.showDone = true;
    ui.pin = '';
    $('pin').value = '';
  }
  const pairingView = !paired || ui.pairing || ui.showDone;
  showIf($('shell'), !pairingView);
  showIf($('pairing'), pairingView);
  if (pairingView) renderPairing(v);
  else renderShell(v);
}

function renderShell(v) {
  const t = totals(v);
  setText($('brand-ver'), `Version ${v.version}`);

  const idx = PAGE_ORDER.indexOf(ui.page);
  setStyle($('nav-hl'), 'transform', `translateY(${idx * 46}px)`);
  document.querySelectorAll('.nav button').forEach((b) => setAttr(b, 'aria-current', b.dataset.page === ui.page ? 'page' : null));
  const on = v.monitors.filter((m) => monitorView(v, m).enabled).length;
  showIf($('badge-on'), v.monitors.length > 0);
  setText($('badge-on'), `${on} on`);

  setDot($('side-dot'), v);
  setText($('side-name'), v.paired_name || 'Your PC');
  setText($('side-line'), stateLine(v));
  bars($('side-bars'), hist.total, (x) => (x / (peak(hist.total, 1) * 1.25)) * 100, '#D4D4D4', t.live > 0);
  tweenText($('side-mbps'), t.mbps);
  tweenText($('side-ms'), t.ms);

  if (ui.shownPage !== ui.page) {
    const [title, sub] = PAGES[ui.page];
    setText($('title'), title);
    setText($('subtitle'), sub);
    replay($('title'), 'fade');
    replay($('subtitle'), 'fade');
    for (const p of PAGE_ORDER) showIf($(`page-${p}`), p === ui.page);
    $('main').scrollTop = 0;
    ui.shownPage = ui.page;
  }
  const pc = v.paired_name || 'the PC';
  setDot($('pill-dot'), v);
  setText($('pill-text'), v.state === 'connected' ? `Connected to ${pc}` : v.state === 'connecting' ? `Connecting to ${pc}…` : `Looking for ${pc}…`);
  setText($('pill-ms'), v.state === 'connected' && t.ms != null ? `≈ ${t.ms.toFixed(0)} ms` : '');

  showIf($('perm'), !v.screen_recording);
  let msg = ui.error;
  if (!msg && v.state === 'connected' && !v.receiver_hevc_ok) msg = 'The PC’s graphics driver reports no HEVC decoding, so it can’t show the stream.';
  if (!msg && v.state !== 'connected' && v.message) msg = v.message;
  showIf($('message'), !!msg);
  setText($('message-text'), msg);

  if (ui.page === 'displays') renderDisplays(v);
  else if (ui.page === 'connection') renderConnection(v, t);
  else renderPrefs(v);
}

// ---------- Displays ----------

let diagramKey = '';

/** Lays the PC monitors out in a row at true relative size, bottoms aligned. */
function layout(mons) {
  const n = mons.length;
  const gap = 16;
  const left = 440;
  const right = 985;
  const bottom = 231;
  const sumW = mons.reduce((a, m) => a + m.width, 0);
  const tallest = Math.max(...mons.map((m) => m.height));
  const s = Math.min((right - left - gap * (n - 1)) / sumW, 150 / tallest);
  let x = left + (right - left - (sumW * s + gap * (n - 1))) / 2;
  const lane = n > 1 ? Math.min(14, 44 / (n - 1)) : 0;
  const port = n > 1 ? Math.min(10, 40 / (n - 1)) : 0;
  return mons.map((m, i) => {
    const w = m.width * s;
    const hh = m.height * s;
    const g = { x, y: bottom - hh, w, h: hh, cx: x + w / 2, py: 206 - (port * (n - 1)) / 2 + i * port, by: 274 + i * lane };
    x += w + gap;
    return g;
  });
}

function buildDiagram(mons) {
  const geo = layout(mons);
  for (const id of ['beams-base', 'beams-line', 'beams-flow', 'ports', 'stands', 'tiles']) $(id).replaceChildren();
  return mons.map((m, i) => {
    const g = geo[i];
    const d = `M 292 ${g.py} C 340 ${g.py} 352 ${g.by} 404 ${g.by} L ${g.cx - 18} ${g.by} Q ${g.cx} ${g.by} ${g.cx} ${g.by - 18} L ${g.cx} 256`;
    const base = svg('path', { d, fill: 'none', stroke: 'rgba(255,255,255,0.09)', 'stroke-width': 1.5 });
    const line = svg('path', { d, class: 'beam-line' });
    const flow = svg('path', { d, class: 'beam-flow' });
    const port = svg('circle', { cx: 292, cy: g.py, r: 3.5, class: 'port' });
    const baseW = Math.min(64, Math.max(40, g.w * 0.45));
    $('beams-base').append(base);
    $('beams-line').append(line);
    $('beams-flow').append(flow);
    $('ports').append(port);
    $('stands').append(
      svg('rect', { x: g.cx - 6, y: 231, width: 12, height: 20, fill: '#1C1C1C' }),
      svg('rect', { x: g.cx - baseW / 2, y: 250, width: baseW, height: 5, rx: 2.5, fill: '#262626' }),
    );
    const tile = h('button', {
      class: 'tile',
      style: `left: ${g.x / 10}%; top: ${g.y / 3.3}%; width: ${g.w / 10}%; height: ${g.h / 3.3}%`,
      onclick: () => select(m.id),
    },
    h('span', { class: 'gloss', 'aria-hidden': 'true' }),
    h('span', { class: 'sheen', 'aria-hidden': 'true', style: `animation-delay: ${(i * 1.4).toFixed(1)}s` }),
    h('span', { class: 'role', 'aria-hidden': 'true' }),
    h('span', { class: 'tname' }),
    h('span', { class: 'tstat' }, h('span', { class: 'tdot' }), h('span', { class: 'tstat-text' })));
    $('tiles').append(tile);
    return { id: m.id, base, line, flow, port, tile };
  });
}
let diagram = [];

function select(id) {
  if (ui.selected === id) return;
  ui.selected = id;
  if (current) render(current);
}

function renderDisplays(v) {
  const mons = v.monitors;
  const pc = v.paired_name || 'the PC';
  setText($('pc-label'), v.paired_name || 'Your PC');
  setText($('pc-sub'), [v.receiver_gpu, mons.length ? plural(mons.length, 'monitor') : null].filter(Boolean).join(' · '));
  const extended = v.streams.filter((s) => !s.mirror).length;
  const mac = v.mac_name.length > 30 ? `${v.mac_name.slice(0, 29)}…` : v.mac_name;
  setText($('mac-sub'), extended ? `${mac} · +${plural(extended, 'display')}` : mac);

  showIf($('arrange-empty'), !mons.length);
  showIf($('mon-cards'), mons.length > 0);
  if (!mons.length) {
    setText($('arrange-empty-text'), v.state === 'connected' ? 'The PC reported no monitors.' : `Monitors appear once ${pc} is connected.`);
    if (diagramKey) {
      diagram = buildDiagram([]);
      diagramKey = '';
    }
    return;
  }
  const key = mons.map((m) => `${m.id}:${m.width}x${m.height}`).join('|');
  if (key !== diagramKey) {
    diagram = buildDiagram(mons);
    diagramKey = key;
  }
  if (!mons.some((m) => m.id === ui.selected)) {
    ui.selected = (mons.find((m) => m.enabled) || mons[0]).id;
  }

  mons.forEach((m, i) => {
    const d = diagram[i];
    const p = monitorView(v, m);
    const s = streamOf(v, m.id);
    const eff = effMode(m, s);
    const color = eff === 'sharp' ? SHARP : FAST;
    const live = !!(p.enabled && s && s.live);
    const starting = p.enabled && !live;
    const waitWord = v.state === 'connected' ? 'Starting' : 'Waiting';
    setAttr(d.base, 'stroke-dasharray', p.enabled ? 'none' : '3 6');
    setAttr(d.line, 'stroke', p.enabled ? color : '#3A3A3A');
    setStyle(d.line, 'opacity', p.enabled ? 0.3 : 0);
    setAttr(d.flow, 'stroke', color);
    setAttr(d.flow, 'stroke-width', eff === 'sharp' ? 3.5 : 2.5);
    setAttr(d.flow, 'stroke-dasharray', eff === 'sharp' ? '0 14' : '5 9');
    setStyle(d.flow, 'opacity', live ? 1 : 0);
    setStyle(d.flow, 'animation-duration', eff === 'sharp' ? '1.1s' : '.7s');
    setStyle(d.flow, 'filter', `drop-shadow(0 0 3px ${eff === 'sharp' ? 'rgba(255,255,255,0.55)' : 'rgba(255,255,255,0.12)'})`);
    setAttr(d.port, 'fill', p.enabled ? color : '#2A2A2A');

    const tile = d.tile;
    toggle(tile, 'off', !p.enabled);
    toggle(tile, 'starting', starting);
    setAttr(tile, 'aria-pressed', m.id === ui.selected ? 'true' : 'false');
    setAttr(tile, 'aria-label', `${m.name}, ${p.enabled ? 'on' : 'off'}`);
    setText(tile.querySelector('.tname'), m.name);
    setText(tile.querySelector('.role'), p.source === 'extend' ? 'Extended' : 'Mirror');
    setText(tile.querySelector('.tstat-text'), starting ? waitWord : live ? `${s.fps_presented.toFixed(0)} fps` : 'Off');
    setStyle(tile.querySelector('.tdot'), 'background-color', p.enabled ? color : '#3A3A3A');
  });

  const m = mons.find((x) => x.id === ui.selected);
  if (ui.shownSelected !== m.id) {
    if (ui.shownSelected != null) {
      replay($('set-card'), 'rise');
      replay($('live-card'), 'rise');
    }
    ui.shownSelected = m.id;
  }
  renderSettings(v, m);
  renderLive(v, m);
}

const MODE_HELP = {
  auto: 'Annex picks for you: the first monitor that’s on gets Sharp, the others Fast, and it drops to Fast when the Mac can’t keep up.',
  sharp: 'Exact colour (4:4:4). Best for text, code and interface.',
  fast: 'Plain 4:2:0 at a lower bitrate. Best for video and motion.',
};

function renderSettings(v, m) {
  const p = monitorView(v, m);
  const s = streamOf(v, m.id);
  const portrait = isPortrait(m);
  toggle($('set-glyph-box'), 'on', p.enabled);
  toggle($('set-glyph'), 'portrait', portrait);
  setText($('set-name'), m.name);
  setText($('set-meta'), `${m.width}×${m.height} · ${m.refresh_hz.toFixed(0)} Hz${m.primary ? ' · Main' : ''}`);
  setSwitch($('set-switch'), p.enabled);

  setSeg($('seg-mode'), p.mode);
  const eff = s && s.live ? `${s.mode} · ${s.mode === 'Sharp' ? '4:4:4' : '4:2:0'}${s.downgraded ? ' · lowered by encoder load' : ''}` : p.enabled ? (v.state === 'connected' ? 'Starting…' : 'Waiting for the PC') : p.mode === 'auto' ? 'Picked when on' : `${p.mode === 'sharp' ? 'Sharp' : 'Fast'} when on`;
  setText($('eff-label'), eff);
  setStyle($('eff-label'), 'color', s && s.mode === 'Sharp' ? SHARP : 'var(--muted)');
  let help = MODE_HELP[p.mode] || MODE_HELP.auto;
  if (p.mode === 'auto' && s && s.live) help += ` This one is ${s.mode} right now.`;
  setText($('mode-help'), help);

  const mirror = p.source !== 'extend';
  setSeg($('seg-src'), mirror ? 'mirror' : 'extend');
  $('seg-src').querySelector('[data-v="mirror"]').disabled = !v.mac_displays.length;
  showIf($('mirror-pick'), mirror && v.mac_displays.length > 1);
  const sel = $('mirror-src');
  if (document.activeElement !== sel) {
    syncList(sel, v.mac_displays, (d) => d.uuid, (d) => h('option', { value: d.uuid }), (el, d) => {
      setText(el, d.name);
      el.value = d.uuid;
    });
    if (mirror && sel.value !== p.source) sel.value = p.source;
  }

  showIf($('shape-field'), mirror);
  if (mirror) {
    setSeg($('seg-shape'), p.match ? 'match' : 'fit');
    const md = v.mac_displays.find((d) => d.uuid === p.source);
    const macA = md ? md.width / md.height : 1.6;
    const pcA = m.width / m.height;
    const box = pcA >= 1 ? [112, Math.round(112 / pcA)] : [Math.round(80 * pcA), 80];
    setStyle($('dia-pc-box'), 'width', `${box[0]}px`);
    setStyle($('dia-pc-box'), 'height', `${box[1]}px`);
    let macPic = [100, 100];
    let pcPic = [100, 100];
    if (!p.match) pcPic = pcA > macA ? [(macA / pcA) * 100, 100] : [100, (pcA / macA) * 100];
    else macPic = pcA > macA ? [100, (macA / pcA) * 100] : [(pcA / macA) * 100, 100];
    setStyle($('dia-mac'), 'width', `${macPic[0]}%`);
    setStyle($('dia-mac'), 'height', `${macPic[1]}%`);
    setStyle($('dia-pc'), 'width', `${pcPic[0]}%`);
    setStyle($('dia-pc'), 'height', `${pcPic[1]}%`);
    setText($('shape-help'), p.match
      ? 'Match is sharper and fills the PC monitor. Your Mac’s own screen gets the bars instead. Best when the PC monitor is your main screen.'
      : 'Fit keeps your Mac’s shape, so the PC monitor shows black bars.');
  }
}

const STATS = [
  { key: 'fps', label: 'Frame rate', unit: 'fps', digits: 0 },
  { key: 'mbps', label: 'Bitrate', unit: 'Mbps', digits: 0 },
  { key: 'ms', label: 'Est. latency', unit: 'ms', digits: 0 },
  { key: 'loss', label: 'Loss', unit: '%', digits: 2 },
];

function renderLive(v, m) {
  const p = monitorView(v, m);
  const s = streamOf(v, m.id);
  const live = !!(p.enabled && s && s.live);
  const chip = $('chip');
  const chipClass = !p.enabled ? 'chip' : !live ? 'chip starting' : `chip on${s.mode === 'Sharp' ? ' sharp' : ''}`;
  if (chip.className !== chipClass) chip.className = chipClass;
  setText($('chip-text'), !p.enabled ? 'Off' : !live ? (v.state === 'connected' ? 'Starting…' : 'Waiting for the PC') : `${s.mode} · ${s.mode === 'Sharp' ? '4:4:4' : '4:2:0'}`);

  showIf($('live-on'), p.enabled);
  showIf($('live-off'), !p.enabled);
  showIf($('live-foot'), p.enabled);
  if (!p.enabled) return;
  setText($('live-foot'), live
    ? `HEVC · ${s.width}×${s.height} · encode ${s.encode_ms.toFixed(1)} ms · FEC ${s.fec_percent}%`
    : v.state === 'connected' ? 'Waiting for the first frame…' : 'The display stays plugged in while Annex reconnects.');
  showIf($('live-warn'), !!(s && s.warning));
  setText($('live-warn'), s && s.warning ? s.warning : '');

  const grid = $('stats');
  if (!grid.children.length) {
    for (const st of STATS) {
      grid.append(h('div', { class: 'stat', 'data-k': st.key },
        h('span', { class: 'eyebrow' }, st.label),
        h('div', { class: 'stat-val' }, h('span', { class: 'num mono' }, '—'), h('span', { class: 'unit' }, st.unit)),
        h('div', { class: 'bars mid', 'aria-hidden': 'true' })));
    }
  }
  const hm = hist.mon.get(m.id) || { fps: [], mbps: [], ms: [], loss: [] };
  const color = s && s.mode === 'Sharp' ? SHARP : FAST;
  const values = live ? { fps: s.fps_presented, mbps: s.bitrate_mbps, ms: s.e2e_ms, loss: s.loss_percent } : {};
  const hz = Math.max(1, m.refresh_hz);
  const norm = {
    fps: (x) => ((x - hz / 2) / (hz / 2)) * 100,
    mbps: (x) => (x / (peak(hm.mbps, 1) * 1.25)) * 100,
    ms: (x) => (x / (peak(hm.ms, 20) * 1.3)) * 100,
    loss: (x) => (x / (peak(hm.loss, 0.5) * 1.3)) * 100,
  };
  for (const st of STATS) {
    const el = grid.querySelector(`[data-k="${st.key}"]`);
    tweenText(el.querySelector('.num'), values[st.key] ?? null, st.digits);
    bars(el.querySelector('.bars'), hm[st.key], norm[st.key], color, live);
  }
}

// ---------- Connection ----------

function renderConnection(v, t) {
  const connected = v.state === 'connected';
  setText($('c-name'), v.paired_name || 'Your PC');
  setText($('c-gpu'), v.receiver_gpu ? `${v.receiver_gpu} · Windows` : 'Windows');
  setDot($('c-dot'), v);
  setText($('c-state'), connected ? 'Connected' : v.state === 'connecting' ? 'Connecting…' : 'Looking for it…');
  setText($('c-hevc'), !connected ? '—' : v.receiver_hevc_ok ? 'Supported by the PC’s graphics' : 'Not reported by the PC’s driver');
  setText($('c-addr'), v.address || '—');
  setText($('c-ports'), v.ports || '—');
  tweenText($('l-mbps'), t.mbps);
  tweenText($('l-ms'), t.ms);
  tweenText($('l-loss'), t.loss, 2);
  bars($('link-bars'), hist.total, (x) => (x / (peak(hist.total, 1) * 1.25)) * 100, '#D4D4D4', t.live > 0);
  showIf($('forget'), !ui.forgetAsk);
  showIf($('forget-confirm'), ui.forgetAsk);
  setText($('forget-name'), v.paired_name || 'this PC');
}

// ---------- Preferences ----------

function renderPrefs(v) {
  setSwitch($('cursor'), withPending('cursor', v.client_cursor));
  setSwitch($('autostart'), withPending('autostart', v.autostart));
  const ok = v.screen_recording;
  toggle($('sr-chip'), 'no', !ok);
  setAttr($('sr-icon'), 'd', ok ? 'M5 12.5l4.5 4.5L19 7.5' : 'M7 7l10 10 M17 7L7 17');
  setText($('sr-text'), ok ? 'Allowed' : 'Not allowed');
  setText($('about-ver'), `Annex ${v.version}`);
  setText($('log-path'), v.log_path.replace(/^\/Users\/[^/]+/, '~'));
}

// ---------- Pair with a PC ----------

/** A stable spot on the radar for a PC, from its id. */
function radarSpot(id) {
  let x = 2166136261;
  for (const c of id) x = Math.imul(x ^ c.charCodeAt(0), 16777619) >>> 0;
  const angle = ((x % 360) * Math.PI) / 180;
  const r = 30 + ((x >>> 9) % 12);
  return { x: 50 + r * Math.cos(angle), y: 50 + r * Math.sin(angle) };
}

let shownPairing = '';

function renderPairing(v) {
  const done = ui.showDone;
  const busy = ui.attempt === 'sent';
  const failed = ui.attempt === 'failed';

  // Steps.
  const sr = v.screen_recording;
  const step1 = sr ? 'step done' : 'step current';
  if ($('step1').className !== step1) $('step1').className = step1;
  if (shownPairing !== String(sr)) {
    $('step1-sub').replaceChildren(sr ? 'Allowed' : h('button', { class: 'textlink', onclick: () => invoke('open_screen_recording_settings') }, 'Allow in System Settings'));
    shownPairing = String(sr);
  }
  toggle($('bar1'), 'full', sr);
  const step2 = done ? 'step done' : 'step current';
  if ($('step2').className !== step2) $('step2').className = step2;
  setText($('step2-sub'), done ? `Paired with ${v.paired_name || 'your PC'}` : 'Type the PIN from the PC');
  toggle($('bar2'), 'full', done);
  const step3 = done ? 'step current' : 'step';
  if ($('step3').className !== step3) $('step3').className = step3;

  // Radar.
  const found = v.discovered;
  setText($('found-text'), found.length ? `${found.length} found` : 'Searching…');
  toggle($('radar'), 'done', done);
  if (!found.some((r) => r.id === ui.pc)) ui.pc = (found.find((r) => r.pairing) || found[0] || {}).id ?? null;
  const chosenId = done ? v.paired_id : ui.pc;
  syncList($('blips'), found, (r) => r.id, (r) => {
    const at = radarSpot(r.id);
    return h('span', { class: 'blip', style: `left: ${at.x}%; top: ${at.y}%` },
      h('span', null, h('span', { class: 'pt' }, h('i'), h('i')), h('span', { class: 'tag' })));
  }, (el, r) => {
    toggle(el, 'on', r.id === chosenId);
    setText(el.querySelector('.tag'), r.name);
  });
  const chosen = found.find((r) => r.id === chosenId);
  showIf($('linkline'), done && !!chosen);
  if (done && chosen) {
    const at = radarSpot(chosen.id);
    setAttr($('linkline-path'), 'd', `M 50 50 L ${at.x} ${at.y}`);
  }
  setText($('scan-text'), done
    ? `Connected. Annex finds ${v.paired_name || 'the PC'} by itself from now on.`
    : found.length ? 'Still listening. A PC shows up here as soon as Annex is running on it.' : 'Looking for PCs running Annex on this network…');

  showIf($('pair-back'), v.state !== 'not_paired' && !done);
  showIf($('pair-form-view'), !done);
  showIf($('pair-done-view'), done);
  setText($('paired-title'), `Paired with ${v.paired_name || 'your PC'}`);
  if (done) return;

  // PCs.
  const pcs = $('pcs');
  if (!found.length) {
    if (!pcs.querySelector('.pc-none')) pcs.replaceChildren(h('div', { class: 'pc-none' }, h('i', { 'aria-hidden': 'true' }), 'Looking for PCs…'));
  } else {
    const none = pcs.querySelector('.pc-none');
    if (none) none.remove();
    syncList(pcs, found, (r) => r.id, (r) => {
      const input = h('input', { type: 'radio', name: 'pc', value: r.id });
      input.addEventListener('change', () => {
        ui.pc = r.id;
        if (ui.attempt === 'failed') ui.attempt = 'none';
        ui.pinMsg = '';
        render(current);
      });
      return h('label', { class: 'pc-opt' }, input, h('span', { class: 'radio', 'aria-hidden': 'true' }, h('i')),
        h('span', { class: 'grow' }, h('span', { class: 'pname' })),
        h('span', { class: 'pin-chip' }, h('i', { 'aria-hidden': 'true' }), 'Showing a PIN'));
    }, (el, r) => {
      const on = r.id === ui.pc;
      toggle(el, 'on', on);
      el.querySelector('input').checked = on;
      setText(el.querySelector('.pname'), r.name);
      showIf(el.querySelector('.pin-chip'), r.pairing);
    });
  }

  // PIN.
  const digits = ui.pin.split('');
  const active = ui.pinFocused && !busy ? Math.min(digits.length, 5) : -1;
  [...$('cells').querySelectorAll('.cell')].forEach((c, i) => {
    const ch = digits[i] || '';
    const b = c.querySelector('b');
    if (ch && (!b || b.textContent !== ch)) c.replaceChildren(h('b', null, ch));
    else if (!ch && b) c.replaceChildren();
    toggle(c, 'filled', !!ch);
    toggle(c, 'active', i === active);
    toggle(c, 'caret', i === active && !ch);
  });
  toggle($('cells'), 'bad', failed || !!ui.pinMsg);
  const picked = found.find((r) => r.id === ui.pc);
  const idle = !found.length
    ? 'Waiting for a PC to show up…'
    : picked && picked.pairing ? `${picked.name} is showing a PIN.` : `Choose Pair with a Mac… on ${picked ? picked.name : 'the PC'} first.`;
  const msg = failed ? v.pairing_text || 'Pairing failed.' : ui.pinMsg || idle;
  setText($('pin-msg'), msg);
  toggle($('pin-msg'), 'bad', failed || !!ui.pinMsg);
  toggle($('pair-btn'), 'ready', digits.length === 6 || busy);
  toggle($('pair-btn'), 'busy', busy);
  $('pair-btn').disabled = busy;
  setText($('pair-btn-text'), busy ? 'Pairing…' : 'Pair');
}

// ---------- events ----------

async function refresh() {
  try {
    const v = await invoke('status');
    record(v);
    render(v);
  } catch (e) {
    ui.error = String(e);
  }
}

function act(promise) {
  if (current) render(current);
  Promise.resolve(promise)
    .then(() => { ui.error = ''; })
    .catch((e) => { ui.error = String(e); })
    .finally(() => setTimeout(refresh, 300));
}

function selectedMonitor() {
  return current && current.monitors.find((m) => m.id === ui.selected);
}

document.querySelectorAll('.nav button').forEach((b) => b.addEventListener('click', () => go(b.dataset.page)));

$('set-switch').addEventListener('click', () => {
  const m = selectedMonitor();
  if (m) act(setMonitor(m.id, 'enabled', !monitorView(current, m).enabled));
});
$('turn-on').addEventListener('click', () => {
  const m = selectedMonitor();
  if (m) act(setMonitor(m.id, 'enabled', true));
});
$('seg-mode').addEventListener('click', (e) => {
  const b = e.target.closest('button');
  const m = selectedMonitor();
  if (b && m) act(setMonitor(m.id, 'mode', b.dataset.v));
});
$('seg-src').addEventListener('click', (e) => {
  const b = e.target.closest('button');
  const m = selectedMonitor();
  if (!b || !m || b.disabled) return;
  const now = monitorView(current, m).source;
  if (b.dataset.v === 'extend' && now !== 'extend') act(setMonitor(m.id, 'source', 'extend'));
  else if (b.dataset.v === 'mirror' && now === 'extend') {
    const uuid = defaultMirror(current);
    if (uuid) act(setMonitor(m.id, 'source', uuid));
  }
});
$('mirror-src').addEventListener('change', (e) => {
  const m = selectedMonitor();
  e.target.blur();
  if (m) act(setMonitor(m.id, 'source', e.target.value));
});
$('seg-shape').addEventListener('click', (e) => {
  const b = e.target.closest('button');
  const m = selectedMonitor();
  if (b && m) act(setMonitor(m.id, 'match', b.dataset.v === 'match'));
});
$('open-displays').addEventListener('click', () => invoke('open_displays_settings'));
$('perm-open').addEventListener('click', () => invoke('open_screen_recording_settings'));
$('sr-open').addEventListener('click', () => invoke('open_screen_recording_settings'));
$('cursor').addEventListener('click', () => {
  const on = !withPending('cursor', current.client_cursor);
  setPending('cursor', on);
  act(invoke('set_client_cursor', { on }));
});
$('autostart').addEventListener('click', () => {
  const on = !withPending('autostart', current.autostart);
  setPending('autostart', on);
  act(invoke('set_autostart', { on }).catch((err) => {
    pending.delete('autostart');
    throw new Error(`Could not change the login item: ${err}`);
  }));
});
$('reveal-log').addEventListener('click', () => invoke('reveal_log'));
$('report').addEventListener('click', () => invoke('report_problem'));
$('repair').addEventListener('click', () => go('pair'));
// The web view has no confirm() dialog, so Forget asks inline.
$('forget').addEventListener('click', () => {
  ui.forgetAsk = true;
  render(current);
});
$('forget-no').addEventListener('click', () => {
  ui.forgetAsk = false;
  render(current);
});
$('forget-yes').addEventListener('click', () => {
  ui.forgetAsk = false;
  ui.pairing = false;
  act(invoke('forget'));
});

$('pair-back').addEventListener('click', () => go(ui.page));
$('paired-go').addEventListener('click', () => {
  ui.showDone = false;
  go('displays');
});
const pin = $('pin');
pin.addEventListener('input', () => {
  ui.pin = pin.value.replace(/\D/g, '').slice(0, 6);
  pin.value = ui.pin;
  ui.pinMsg = '';
  if (ui.attempt === 'failed') ui.attempt = 'none';
  render(current);
});
pin.addEventListener('focus', () => {
  ui.pinFocused = true;
  render(current);
});
pin.addEventListener('blur', () => {
  ui.pinFocused = false;
  render(current);
});
$('pin-form').addEventListener('submit', (e) => {
  e.preventDefault();
  if (ui.attempt === 'sent') return;
  if (ui.pin.length !== 6) {
    ui.pinMsg = 'The PIN has 6 digits.';
    replay($('cells'), 'shake');
    render(current);
    return;
  }
  ui.attempt = 'sent';
  ui.sentAt = Date.now();
  ui.sawWorking = false;
  ui.pinMsg = '';
  act(invoke('pair', { receiverId: ui.pc, pin: ui.pin }).catch((err) => {
    ui.attempt = 'none';
    ui.pinMsg = String(err);
  }));
});

const start = new URLSearchParams(location.search).get('page');
if (start) go(start);
refresh();
setInterval(refresh, 1000);
