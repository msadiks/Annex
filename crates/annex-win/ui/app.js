"use strict";
// The Annex window. Rust pushes the whole state with annex.update(state) and
// the page sends commands with send({cmd, ...}) (see GuiCmd in src/app.rs).
// Elements are created once and updated in place so animations and focus
// survive the once-a-second refresh.

const $ = (id) => document.getElementById(id);
const send = (msg) => window.ipc && window.ipc.postMessage(JSON.stringify(msg));

function el(tag, props, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const k of kids) e.append(k);
  return e;
}

function setText(node, text) {
  if (node.textContent !== text) node.textContent = text;
}

function button(label, cmd, primary) {
  return el("button", { class: primary ? "btn primary" : "btn", text: label, onclick: () => send(cmd) });
}

let state = null;
let pinTimer = null;

// --- Header and hero -------------------------------------------------------

function renderStatus(s) {
  setText($("host"), `This PC: ${s.host}`);
  const chip = $("chip");
  const hero = $("hero");
  let key = s.status, chipText, title, text;
  const actions = [];
  if (s.pairing) {
    key = "pairing";
    chipText = "Pairing";
    title = "Waiting for your Mac";
    text = "Enter the PIN below in Annex on your Mac.";
  } else if (s.status === "connected") {
    chipText = "Connected";
    title = `Connected to ${s.peer}`;
    const live = s.monitors.filter((m) => m.stream).length;
    text = live ? `${live} ${live === 1 ? "display" : "displays"} streaming from your Mac.`
      : "Turn on a monitor below to use it as a Mac display.";
  } else if (s.status === "waiting") {
    chipText = "Waiting";
    title = `Looking for ${s.peer}`;
    text = "Annex connects by itself when your Mac is on the same network with Annex open.";
  } else {
    chipText = "Not paired";
    title = "Pair with your Mac";
    text = "Install Annex on your Mac, then pair once. They find each other on your network from then on.";
    actions.push(button("Pair with a Mac", { cmd: "pair" }, true));
  }
  chip.className = `chip ${key}`;
  setText($("chip-text"), chipText);
  hero.className = `card hero ${key}`;
  setText($("hero-title"), title);
  setText($("hero-text"), text);
  const box = $("hero-actions");
  const sig = actions.map((a) => a.textContent).join("|");
  if (box.dataset.sig !== sig) {
    box.dataset.sig = sig;
    box.replaceChildren(...actions);
  }
}

// --- Banners -----------------------------------------------------------------

const FIREWALL = {
  norule: {
    title: "Windows Firewall needs to let your Mac in",
    text: "Your Mac connects to this PC over the network, and Windows Firewall blocks it until Annex is allowed. Windows will ask for permission once.",
    actions: [["Allow Annex", { cmd: "firewall" }, true]],
  },
  blocked: {
    title: "Windows Firewall is blocking Annex",
    text: "A firewall rule stops your Mac from reaching this PC (this happens if the Windows Firewall prompt was cancelled). Allow Annex to fix it.",
    actions: [["Allow Annex", { cmd: "firewall" }, true]],
  },
  public: {
    title: "Windows Firewall blocks Annex on this network",
    text: "This PC's network is set to Public, and Annex is only allowed on private networks. Allow it for devices on your local network, or set the network to Private in Windows Settings.",
    actions: [
      ["Allow on my local network", { cmd: "firewall" }, true],
      ["Open network settings", { cmd: "networkSettings" }, false],
    ],
  },
};

function renderBanners(s) {
  const fw = FIREWALL[s.firewall];
  const box = $("firewall");
  box.hidden = !fw;
  if (fw && box.dataset.key !== s.firewall) {
    box.dataset.key = s.firewall;
    setText($("fw-title"), fw.title);
    setText($("fw-text"), fw.text);
    $("fw-actions").replaceChildren(...fw.actions.map(([l, c, p]) => button(l, c, p)));
  }

  let alert = null, bad = false;
  if (s.gpu && s.gpu.name && !s.gpu.hevc) {
    alert = `This PC's graphics (${s.gpu.name}) can't decode HEVC video in hardware, which Annex needs.`;
    bad = true;
  } else if (s.warning) {
    alert = s.warning;
  }
  $("alert").hidden = !alert;
  $("alert").className = `card banner ${bad ? "bad" : "warn"}`;
  if (alert) setText($("alert-text"), alert);
}

// --- Pairing -----------------------------------------------------------------

function renderPairing(s) {
  const card = $("pairing");
  const p = s.pairing;
  const appeared = card.hidden && !!p;
  card.hidden = !p;
  clearInterval(pinTimer);
  if (!p) return;
  // Pairing can start from the footer link, far below the PIN.
  if (appeared) card.scrollIntoView({ behavior: "smooth", block: "center" });
  const pin = $("pin");
  if (pin.dataset.pin !== p.pin) {
    pin.dataset.pin = p.pin;
    pin.replaceChildren(...[...p.pin].map((c) => el("span", { class: c === " " ? "gap" : "", text: c === " " ? "" : c })));
  }
  const ends = Date.now() + p.secondsLeft * 1000;
  const tick = () => {
    const left = Math.max(0, Math.round((ends - Date.now()) / 1000));
    setText($("pin-left"), `Expires in ${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`);
    $("meter-fill").style.width = `${(100 * left) / p.total}%`;
  };
  tick();
  pinTimer = setInterval(tick, 1000);
}

function renderNotice(s) {
  const n = s.notice;
  const card = $("notice");
  const sig = n ? `${n.ok}|${n.title}|${n.text}` : "";
  if (card.dataset.sig === sig) return;
  card.dataset.sig = sig;
  card.hidden = !n;
  if (!n) return;
  card.className = `card notice ${n.ok ? "" : "bad"}`;
  $("mark-path").setAttribute("d", n.ok ? "M7 12.5l3.2 3.2L17 9" : "M8.5 8.5l7 7M15.5 8.5l-7 7");
  setText($("notice-title"), n.title);
  setText($("notice-text"), n.text);
  // Restart the entrance animation.
  card.style.animation = "none";
  void card.offsetWidth;
  card.style.animation = "";
}

// --- Monitors ----------------------------------------------------------------

const cards = new Map();
// Mac display names by uuid, kept while the Mac is away so a mirrored
// monitor still says what it mirrors.
const macNames = new Map();
const MODES = [["auto", "Auto"], ["sharp", "Sharp"], ["fast", "Fast"]];

function monitorCard(m) {
  const toggle = el("input", { type: "checkbox", role: "switch", "aria-label": `Use ${m.name} as a Mac display` });
  toggle.addEventListener("change", () => send({ cmd: "monitor", id: m.id, enabled: toggle.checked }));

  const thumb = el("div", { class: "thumb" });
  const seg = el("div", { class: "seg", role: "radiogroup", "aria-label": "Mode" }, thumb);
  for (const [key, label] of MODES) {
    const b = el("button", { text: label, "data-mode": key, role: "radio" });
    b.addEventListener("click", () => send({ cmd: "mode", id: m.id, mode: key }));
    seg.append(b);
  }
  const source = el("select", { "aria-label": "Source" });
  source.addEventListener("change", () => send({ cmd: "source", id: m.id, source: source.value }));

  const name = el("b");
  const meta = el("small");
  const stats = el("div", { class: "stats" });
  const glyph = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  glyph.setAttribute("viewBox", "0 0 34 26");
  glyph.setAttribute("class", "mon-glyph");
  glyph.innerHTML = '<rect class="screen" x="2" y="2" width="30" height="18" rx="2"/>' +
    '<rect x="2" y="2" width="30" height="18" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/>' +
    '<path d="M14 20h6v3h-6zM10 23h14v1.6H10z" fill="currentColor"/>';

  const root = el("div", { class: "card monitor" },
    el("div", { class: "mon-head" }, glyph, el("div", { class: "mon-name" }, name, meta), toggle),
    el("div", { class: "mon-more" }, el("div", {},
      el("div", { class: "mon-settings" },
        el("div", { class: "field" }, el("span", { text: "Mode", "aria-hidden": "true" }), seg),
        el("label", { class: "field grow" }, "Source", source)),
      stats)));
  return { root, toggle, seg, thumb, source, name, meta, stats };
}

function placeThumb(c, mode) {
  const on = c.seg.querySelector(`button[data-mode="${mode}"]`);
  c.seg.querySelectorAll("button").forEach((b) => {
    b.classList.toggle("on", b === on);
    b.setAttribute("aria-checked", b === on ? "true" : "false");
  });
  if (!on || !on.offsetWidth) return;
  c.thumb.style.width = `${on.offsetWidth}px`;
  c.thumb.style.transform = `translateX(${on.offsetLeft}px)`;
}

function updateCard(c, m, s) {
  setText(c.name, m.name);
  setText(c.meta, `${m.width}×${m.height} · ${m.refresh} Hz${m.primary ? " · Main display" : ""}`);
  c.toggle.checked = m.enabled;
  c.root.classList.toggle("on", m.enabled);
  c.root.classList.toggle("live", !!m.stream);
  c.mode = m.mode;
  placeThumb(c, m.mode);

  for (const d of s.macDisplays) macNames.set(d.uuid, d.name);
  const opts = [["extend", "Extend: new Mac display"], ...s.macDisplays.map((d) => [d.uuid, `Mirror ${d.name}`])];
  if (m.source !== "extend" && !opts.some(([v]) => v === m.source)) {
    opts.push([m.source, macNames.has(m.source) ? `Mirror ${macNames.get(m.source)}` : "Mirror a Mac display"]);
  }
  const sig = opts.map((o) => o.join("=")).join("|");
  if (c.source.dataset.sig !== sig) {
    c.source.dataset.sig = sig;
    c.source.replaceChildren(...opts.map(([v, l]) => el("option", { value: v, text: l })));
  }
  if (document.activeElement !== c.source) c.source.value = m.source;

  const st = m.stats, sm = m.stream;
  if (sm) {
    const parts = [el("span", { class: "live-tag", text: "LIVE" }), el("span", { text: `${sm.mode} ${sm.width}×${sm.height}` })];
    if (st) {
      const add = (v, unit) => parts.push(el("span", { class: "sep", text: "·" }), el("span", {}, el("b", { text: v }), ` ${unit}`));
      add(st.fps.toFixed(0), "fps");
      add(st.mbps.toFixed(1), "Mbps");
      if (st.latency != null) add(String(st.latency), "ms");
      if (st.loss > 0.05) add(st.loss.toFixed(1), "% loss");
    }
    c.stats.replaceChildren(...parts);
  } else if (m.enabled && s.status === "connected") {
    c.stats.replaceChildren(el("span", { text: "Starting…" }));
  } else if (m.enabled) {
    c.stats.replaceChildren(el("span", { text: "Plugs in when your Mac connects." }));
  } else {
    c.stats.replaceChildren();
  }
}

function renderMonitors(s) {
  const box = $("monitors");
  const ids = new Set(s.monitors.map((m) => m.id));
  for (const [id, c] of cards) {
    if (!ids.has(id)) {
      c.root.remove();
      cards.delete(id);
    }
  }
  s.monitors.forEach((m, i) => {
    let c = cards.get(m.id);
    if (!c) {
      c = monitorCard(m);
      c.root.style.animationDelay = `${i * 50}ms`;
      cards.set(m.id, c);
    }
    if (box.children[i] !== c.root) box.insertBefore(c.root, box.children[i] || null);
    updateCard(c, m, s);
  });
  if (!s.monitors.length && !box.querySelector(".empty")) {
    box.append(el("div", { class: "card empty", text: "No monitors found." }));
  } else if (s.monitors.length) {
    box.querySelectorAll(".empty").forEach((e) => e.remove());
  }
}

// --- Options and footer ------------------------------------------------------

function renderOptions(s) {
  for (const [id, on] of [["opt-autostart", s.options.autostart], ["opt-stats", s.options.stats]]) {
    $(id).checked = on;
  }
  const links = $("peer-links");
  const sig = `${s.peer}|${!!s.pairing}`;
  if (links.dataset.sig !== sig) {
    links.dataset.sig = sig;
    const kids = [];
    if (s.peer) {
      kids.push(el("span", { class: "muted small", text: `Paired with ${s.peer}` }));
      if (!s.pairing) kids.push(el("button", { class: "link", text: "Pair with a different Mac", onclick: () => send({ cmd: "pair" }) }));
      // Two clicks: the first asks.
      const forget = el("button", { class: "link", text: `Forget ${s.peer}` });
      forget.addEventListener("click", () => {
        if (forget.dataset.armed) return send({ cmd: "forget" });
        forget.dataset.armed = "1";
        forget.textContent = "Click again to forget (you'll need to pair again)";
        setTimeout(() => { delete forget.dataset.armed; forget.textContent = `Forget ${s.peer}`; }, 4000);
      });
      kids.push(forget);
    }
    links.replaceChildren(...kids);
  }
  const gpu = s.gpu && s.gpu.name ? ` · ${s.gpu.name}` : "";
  setText($("about"), `Annex ${s.version}${s.portable ? " (portable)" : ""}${gpu}`);
}

// --- Entry points ------------------------------------------------------------

// When the window opens, the web view can put focus on the first control,
// which scrolls the page down to Options. Start at the top instead.
const openedAt = Date.now();
let touched = false;
for (const t of ["pointerdown", "keydown"]) document.addEventListener(t, () => { touched = true; }, true);
document.addEventListener("focusin", (e) => {
  if (touched || Date.now() - openedAt > 2000) return;
  e.target.blur();
  window.scrollTo(0, 0);
});

window.annex = {
  update(s) {
    state = s;
    renderStatus(s);
    renderBanners(s);
    renderPairing(s);
    renderNotice(s);
    renderMonitors(s);
    renderOptions(s);
  },
};

$("opt-autostart").addEventListener("change", (e) => send({ cmd: "autostart", on: e.target.checked }));
$("opt-stats").addEventListener("change", (e) => send({ cmd: "stats", on: e.target.checked }));
document.addEventListener("click", (e) => {
  const t = e.target.closest("[data-cmd],[data-url]");
  if (!t) return;
  if (t.dataset.url) send({ cmd: "open", url: t.dataset.url });
  else send({ cmd: t.dataset.cmd });
});
// The segmented control's thumb needs layout; fonts can shift it.
window.addEventListener("resize", () => cards.forEach((c) => c.mode && placeThumb(c, c.mode)));
document.fonts && document.fonts.ready.then(() => state && annex.update(state));
send({ cmd: "ready" });
