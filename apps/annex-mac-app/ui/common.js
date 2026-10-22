// Helpers shared by the window and the popover. The pages keep their DOM and
// change it in place each second, so CSS transitions and animations keep
// running instead of restarting on every refresh.
'use strict';

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
const $ = (id) => document.getElementById(id);
const SVG_NS = 'http://www.w3.org/2000/svg';
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function build(el, attrs, kids) {
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  for (const kid of kids.flat()) if (kid != null) el.append(kid);
  return el;
}
const h = (tag, attrs, ...kids) => build(document.createElement(tag), attrs, kids);
const svg = (tag, attrs, ...kids) => build(document.createElementNS(SVG_NS, tag), attrs, kids);

function setText(el, t) {
  t = String(t ?? '');
  if (el.textContent !== t) el.textContent = t;
}
function setAttr(el, k, v) {
  if (v == null || v === false) {
    if (el.hasAttribute(k)) el.removeAttribute(k);
    return;
  }
  v = v === true ? '' : String(v);
  if (el.getAttribute(k) !== v) el.setAttribute(k, v);
}
function setStyle(el, k, v) {
  v = String(v);
  if (el.style.getPropertyValue(k) !== v) el.style.setProperty(k, v);
}
function toggle(el, name, on) {
  el.classList.toggle(name, !!on);
}
function showIf(el, on) {
  if (el.hidden === !!on) el.hidden = !on;
}
/** Runs a CSS animation class again. */
function replay(el, name) {
  el.classList.remove(name);
  void el.offsetWidth;
  el.classList.add(name);
}

/** Keeps one child of `parent` per item, in order: `make` builds a new one, `update` refreshes it. */
function syncList(parent, items, key, make, update) {
  const old = new Map();
  for (const c of [...parent.children]) if (c.dataset.key != null) old.set(c.dataset.key, c);
  let prev = null;
  for (const it of items) {
    const k = String(key(it));
    let el = old.get(k);
    if (el) old.delete(k);
    else {
      el = make(it);
      el.dataset.key = k;
    }
    update(el, it);
    const want = prev ? prev.nextSibling : parent.firstChild;
    if (el !== want) parent.insertBefore(el, want);
    prev = el;
  }
  for (const el of old.values()) el.remove();
}

const num = (v, digits = 0) => (v == null || !Number.isFinite(v) ? '—' : v.toFixed(digits));
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

// Numbers glide to their new value instead of jumping.
const tweening = new Set();
let tweenFrame = 0;
function tweenText(el, value, digits = 0, prefix = '') {
  if (value == null || !Number.isFinite(value)) {
    el._tw = null;
    el._val = null;
    tweening.delete(el);
    setText(el, '—');
    return;
  }
  const now = performance.now();
  const from = el._val ?? value;
  if (from === value && !el._tw) {
    setText(el, prefix + value.toFixed(digits));
    return;
  }
  el._tw = { from: currentTween(el, now) ?? from, to: value, t0: now, digits, prefix };
  el._val = value;
  tweening.add(el);
  if (!tweenFrame) tweenFrame = requestAnimationFrame(stepTweens);
}
function currentTween(el, now) {
  const t = el._tw;
  if (!t) return null;
  const k = Math.min(1, (now - t.t0) / 700);
  return t.from + (t.to - t.from) * (1 - Math.pow(1 - k, 3));
}
function stepTweens(now) {
  tweenFrame = 0;
  for (const el of tweening) {
    const t = el._tw;
    if (!t) {
      tweening.delete(el);
      continue;
    }
    const done = reducedMotion || now - t.t0 >= 700;
    setText(el, t.prefix + (done ? t.to : currentTween(el, now)).toFixed(t.digits));
    if (done) {
      el._tw = null;
      tweening.delete(el);
    }
  }
  if (tweening.size) tweenFrame = requestAnimationFrame(stepTweens);
}

// A setting changed here shows at once; the PC confirms it within a moment.
// Until it does (or 3 s pass), the page shows what was picked, not the old value.
const pending = new Map();
function setPending(key, value) {
  pending.set(key, { value, until: Date.now() + 3000 });
}
function withPending(key, actual) {
  const p = pending.get(key);
  if (!p) return actual;
  if (Date.now() > p.until || p.value === actual) {
    pending.delete(key);
    return actual;
  }
  return p.value;
}

// Monitor settings, shared by the window and the popover.
function monitorView(v, m) {
  const id = m.id;
  return {
    enabled: withPending(`${id}:enabled`, m.enabled),
    mode: withPending(`${id}:mode`, m.mode),
    source: withPending(`${id}:source`, m.source),
    match: withPending(`${id}:match`, m.mirror_match),
  };
}
async function setMonitor(id, key, value) {
  setPending(`${id}:${key}`, value);
  try {
    if (key === 'match') await invoke('set_mirror_match', { id, on: value });
    else {
      const args = { id, enabled: null, mode: null, source: null };
      args[key] = value;
      await invoke('set_monitor', args);
    }
  } catch (e) {
    pending.delete(`${id}:${key}`);
    throw e;
  }
}

/** A Mac display to mirror when the user picks Mirror: the built-in one if there is one. */
function defaultMirror(v) {
  const d = v.mac_displays.find((x) => x.builtin) || v.mac_displays[0];
  return d ? d.uuid : null;
}

/** Totals over the live streams: bitrate, average latency, worst loss. */
function totals(v) {
  const live = v.streams.filter((s) => s.live);
  const lat = live.map((s) => s.e2e_ms).filter((x) => x != null);
  return {
    live: live.length,
    mbps: live.length ? live.reduce((a, s) => a + s.bitrate_mbps, 0) : null,
    ms: lat.length ? lat.reduce((a, x) => a + x, 0) / lat.length : null,
    loss: live.length ? Math.max(...live.map((s) => s.loss_percent)) : null,
  };
}

function isPortrait(m) {
  return m.height > m.width;
}
