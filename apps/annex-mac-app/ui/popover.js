// The menu bar popover: the PC, live totals and a card per PC monitor.
'use strict';

let current = null;
let open = null;
let shown = false;
let timer = 0;
let lastHeight = 0;

const MODES = [['auto', 'Automatic'], ['sharp', 'Sharp'], ['fast', 'Fast']];

function sources(v) {
  const one = v.mac_displays.length === 1;
  return [['extend', 'New display']].concat(v.mac_displays.map((d) => [d.uuid, one ? 'Mirror' : `Mirror ${d.name}`]));
}

function makeCard(m) {
  const sw = h('button', { class: 'switch', role: 'switch', 'aria-checked': 'false', 'aria-label': `Use ${m.name} as a Mac display` },
    h('span', { class: 'track' }, h('span', { class: 'knob' })));
  const main = h('button', { class: 'm-main', 'aria-expanded': 'false' },
    h('span', { class: 'm-glyph-box', 'aria-hidden': 'true' }, h('span', { class: `m-glyph${isPortrait(m) ? ' portrait' : ''}` })),
    h('span', { class: 'm-text' }, h('span', { class: 'm-name' }), h('span', { class: 'm-sub' })),
    svg('svg', { class: 'm-chev', width: 13, height: 13, viewBox: '0 0 24 24', 'aria-hidden': 'true' }, svg('path', { d: 'M6 9l6 6l6-6' })));
  const field = (label, k) => h('div', { class: `m-field m-${k}` }, h('span', { class: 'm-label' }, label),
    h('div', { class: 'pseg', role: 'group', 'aria-label': label, 'data-k': k }));
  const more = h('div', { class: 'm-more', hidden: true },
    field('Picture', 'mode'), field('Shows', 'source'), field('Mirror shape', 'match'),
    h('p', { class: 'm-warn', hidden: true }),
    h('div', { class: 'm-foot' }, h('span', { class: 'm-info' }), h('span', { class: 'm-stats' })));
  const card = h('div', { class: 'm-card' }, h('div', { class: 'm-row' }, main, sw), more);

  main.addEventListener('click', () => {
    if (card.classList.contains('waiting')) return;
    open = open === m.id ? null : m.id;
    render(current);
  });
  sw.addEventListener('click', () => {
    const now = monitorView(current, current.monitors.find((x) => x.id === m.id)).enabled;
    act(setMonitor(m.id, 'enabled', !now));
  });
  more.addEventListener('click', (e) => {
    const b = e.target.closest('.pseg button');
    if (!b) return;
    const k = b.parentElement.dataset.k;
    if (k === 'match') act(setMonitor(m.id, 'match', b.dataset.v === 'match'));
    else act(setMonitor(m.id, k, b.dataset.v));
  });
  return card;
}

function setOptions(seg, opts, value) {
  syncList(seg, opts, (o) => o[0], (o) => h('button', { 'data-v': o[0] }), (el, o) => {
    setText(el, o[1]);
    setAttr(el, 'title', o[1]);
    setAttr(el, 'aria-pressed', o[0] === value ? 'true' : 'false');
  });
}

function updateCard(card, v, m, waiting) {
  const p = monitorView(v, m);
  const s = v.streams.find((x) => x.monitor_id === m.id);
  const isOpen = !waiting && open === m.id;
  toggle(card, 'on', p.enabled);
  toggle(card, 'open', isOpen);
  toggle(card, 'waiting', waiting);
  setText(card.querySelector('.m-name'), m.name);
  let sub;
  if (waiting) sub = p.enabled ? 'Waiting for the PC' : `Off · ${m.width}×${m.height}`;
  else if (!p.enabled) sub = `Off · ${m.width}×${m.height} · ${m.refresh_hz.toFixed(0)} Hz`;
  else if (!s || !s.live) sub = 'Starting…';
  else sub = `${s.mode} · ${s.fps_presented.toFixed(0)} fps · ${s.bitrate_mbps.toFixed(0)} Mbps`;
  setText(card.querySelector('.m-sub'), sub);
  const main = card.querySelector('.m-main');
  setAttr(main, 'aria-expanded', isOpen ? 'true' : 'false');
  const sw = card.querySelector('.switch');
  setAttr(sw, 'aria-checked', p.enabled ? 'true' : 'false');
  sw.disabled = waiting;

  const more = card.querySelector('.m-more');
  showIf(more, isOpen);
  if (!isOpen) return;
  setOptions(more.querySelector('[data-k="mode"]'), MODES, p.mode);
  setOptions(more.querySelector('[data-k="source"]'), sources(v), p.source);
  const mirror = p.source !== 'extend';
  showIf(more.querySelector('.m-match'), mirror);
  if (mirror) setOptions(more.querySelector('[data-k="match"]'), [['fit', 'Fit'], ['match', 'Match the PC']], p.match ? 'match' : 'fit');
  const warn = more.querySelector('.m-warn');
  showIf(warn, !!(s && s.warning));
  setText(warn, s && s.warning ? s.warning : '');
  setText(more.querySelector('.m-info'), `${m.width}×${m.height} · ${m.refresh_hz.toFixed(0)} Hz${m.primary ? ' · main' : ''}`);
  setText(more.querySelector('.m-stats'), s && s.live
    ? `${s.e2e_ms != null ? `≈ ${s.e2e_ms.toFixed(0)} ms · ` : ''}${s.loss_percent.toFixed(2)}% loss`
    : p.enabled ? 'Starting…' : 'Not streaming');
}

function render(v) {
  current = v;
  const paired = v.state !== 'not_paired';
  const connected = v.state === 'connected';
  const t = totals(v);

  setText($('p-title'), v.paired_name || 'Annex');
  const dot = connected ? 'p-dot' : paired ? 'p-dot wait' : 'p-dot none';
  if ($('p-dot').className !== dot) $('p-dot').className = dot;
  let status = 'Not paired with a PC';
  if (connected) status = t.live ? `Streaming ${plural(t.live, 'display')}` : 'Connected';
  else if (v.state === 'connecting') status = 'Connecting…';
  else if (paired) status = 'Looking for the PC…';
  setText($('p-status'), status);

  showIf($('p-pair'), !paired);
  showIf($('p-open'), paired);
  const grace = v.state !== 'connected' && v.grace_secs != null && v.kept_displays > 0;
  showIf($('p-note'), paired && !connected);
  if (paired && !connected) {
    setText($('p-note-line'), grace ? `Keeping ${plural(v.kept_displays, 'display')} for ${v.grace_secs} s` : 'Waiting for the PC');
    setText($('p-note-sub'), grace
      ? 'So your windows don’t move while Annex reconnects.'
      : v.message || 'Annex reconnects by itself when the PC is on this network.');
  }

  showIf($('p-totals'), connected);
  if (connected) {
    setText($('t-mbps'), num(t.mbps));
    setText($('t-ms'), t.ms == null ? '—' : `≈${t.ms.toFixed(0)}`);
    setText($('t-loss'), num(t.loss, 2));
  }

  const mons = paired ? v.monitors : [];
  showIf($('p-mons-label'), mons.length > 0);
  if (!mons.some((m) => m.id === open)) open = null;
  syncList($('p-mons'), mons, (m) => m.id, makeCard, (el, m) => updateCard(el, v, m, !connected));

  const anyOn = mons.some((m) => monitorView(v, m).enabled);
  const hint = connected && !mons.length ? 'The PC reported no monitors.' : connected && !anyOn ? 'Turn a monitor on to plug it in as a Mac display.' : '';
  showIf($('p-hint'), !!hint);
  setText($('p-hint'), hint);
  reportHeight();
}

function reportHeight() {
  const pop = $('pop');
  const height = Math.ceil(pop.scrollHeight + 2);
  if (Math.abs(height - lastHeight) < 1) return;
  lastHeight = height;
  invoke('popover_size', { height });
}

async function refresh() {
  try {
    render(await invoke('status'));
  } catch (e) {
    console.error(e);
  }
}

function act(promise) {
  if (current) render(current);
  Promise.resolve(promise).catch((e) => console.error(e)).finally(() => setTimeout(refresh, 300));
}

// The page polls only while it is on screen.
window.annexShown = () => {
  shown = true;
  refresh();
  clearInterval(timer);
  timer = setInterval(refresh, 1000);
};
window.annexHidden = () => {
  shown = false;
  open = null;
  clearInterval(timer);
};

const openAnnex = (page) => invoke('open_annex', { page: page || null });
$('p-window').addEventListener('click', () => openAnnex(current && current.state === 'not_paired' ? 'pair' : null));
$('p-open').addEventListener('click', () => openAnnex(null));
$('p-pair-btn').addEventListener('click', () => openAnnex('pair'));
$('p-quit').addEventListener('click', () => invoke('quit'));
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('popover_hide');
  else if (e.metaKey && e.key === ',') openAnnex(null);
  else if (e.metaKey && e.key.toLowerCase() === 'q') invoke('quit');
});
new ResizeObserver(reportHeight).observe($('pop'));
refresh();
