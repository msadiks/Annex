const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
const $ = (id) => document.getElementById(id);
let current = null;
let showPairing = false;

const STATE_TEXT = {
  not_paired: 'Not paired',
  searching: 'Looking for the PC',
  connecting: 'Connecting',
  connected: 'Connected',
};

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function streamLine(s) {
  if (!s) return '';
  const parts = [
    `${s.mode}${s.downgraded ? ' (lowered by encoder load)' : ''}`,
    `${s.width}×${s.height}`,
    s.live ? `${s.fps_presented.toFixed(0)} fps` : 'starting…',
    `${s.bitrate_mbps.toFixed(0)} Mbps`,
    `encode ${s.encode_ms.toFixed(1)} ms`,
    `loss ${s.loss_percent.toFixed(2)}%`,
  ];
  if (s.e2e_ms != null) parts.push(`latency ≈ ${s.e2e_ms.toFixed(0)} ms`);
  let html = `<div class="stats">${esc(parts.join(' · '))}</div>`;
  if (s.warning) html += `<div class="stats warn">${esc(s.warning)}</div>`;
  return html;
}

function renderMonitors(v) {
  const box = $('monitors');
  if (box.contains(document.activeElement)) return; // don't fight the user
  if (!v.monitors.length) {
    box.innerHTML = `<p class="hint">${v.state === 'connected' ? 'The PC reported no monitors.' : 'Monitors appear once the PC is connected.'}</p>`;
    return;
  }
  const mirrorOpts = (sel) =>
    [`<option value="extend"${sel === 'extend' ? ' selected' : ''}>New display (extend)</option>`]
      .concat(v.mac_displays.map((d) => `<option value="${esc(d.uuid)}"${sel === d.uuid ? ' selected' : ''}>Mirror ${esc(d.name)}</option>`))
      .join('');
  box.innerHTML = v.monitors
    .map((m) => {
      const s = v.streams.find((x) => x.monitor_id === m.id);
      return `<div class="monitor" data-id="${m.id}">
        <label class="title"><input type="checkbox" data-k="enabled"${m.enabled ? ' checked' : ''}>
          ${esc(m.name)} <span class="meta">${m.width}×${m.height} · ${m.refresh_hz.toFixed(0)} Hz${m.primary ? ' · main' : ''}</span></label>
        <div class="controls">
          <label>Picture <select data-k="mode">
            <option value="auto"${m.mode === 'auto' ? ' selected' : ''}>Automatic</option>
            <option value="sharp"${m.mode === 'sharp' ? ' selected' : ''}>Sharp (exact colour, text)</option>
            <option value="fast"${m.mode === 'fast' ? ' selected' : ''}>Fast (video, motion)</option>
          </select></label>
          <label>Shows <select data-k="source">${mirrorOpts(m.source)}</select></label>
          ${m.source === 'extend' ? '' : `<label>Mirror shape <select data-k="match">
            <option value="fit"${m.mirror_match ? '' : ' selected'}>Fit, with black bars on the PC</option>
            <option value="match"${m.mirror_match ? ' selected' : ''}>Match the PC monitor (best as your main screen)</option>
          </select></label>`}
        </div>
        ${m.source === 'extend' ? '' : `<div class="stats">Match is sharper and fills the PC monitor, and your Mac's own screen gets the bars instead. Best when the PC monitor is your main screen.</div>`}
        ${m.enabled ? streamLine(s) : ''}
      </div>`;
    })
    .join('');
}

function render(v) {
  current = v;
  const st = $('state');
  st.textContent = STATE_TEXT[v.state] || v.state;
  st.className = `pill ${v.state}`;
  $('perm').hidden = v.screen_recording;
  const pairing = v.state === 'not_paired' || showPairing;
  $('pairing').hidden = !pairing;
  $('displays').hidden = v.state === 'not_paired';
  if (pairing) {
    const found = $('found');
    if (!found.contains(document.activeElement)) {
      const ready = v.discovered.filter((r) => r.pairing);
      found.innerHTML = v.discovered.length
        ? v.discovered
            .map((r, i) => `<label class="row"><input type="radio" name="pc" value="${esc(r.id)}"${(ready[0] ? ready[0].id === r.id : i === 0) ? ' checked' : ''}>
              ${esc(r.name)} <span class="hint">${r.pairing ? '· showing a PIN' : ''}</span></label>`)
            .join('')
        : '<p class="hint">No PC with Annex found yet. Both computers must be on the same network.</p>';
    }
    const res = $('pair-result');
    if (v.pairing === 'working') res.textContent = 'Pairing…';
    else if (v.pairing === 'failed') res.innerHTML = `<span class="bad">${esc(v.pairing_text)}</span>`;
    else if (v.pairing === 'done') {
      res.textContent = `Paired with ${v.pairing_text}.`;
      showPairing = false;
    }
  }
  renderMonitors(v);
  if (document.activeElement !== $('cursor')) $('cursor').checked = v.client_cursor;
  if (document.activeElement !== $('autostart')) $('autostart').checked = v.autostart;
  let msg = v.message || '';
  if (v.state === 'connected' && !v.receiver_hevc_ok) msg = "The PC's graphics driver reports no HEVC decoding.";
  $('message').textContent = msg;
  $('peer').textContent = v.paired_name
    ? `Paired PC: ${v.paired_name}${v.receiver_gpu ? ` (${v.receiver_gpu})` : ''}`
    : '';
  $('forget-box').hidden = !v.paired_name;
  $('forget-name').textContent = v.paired_name || '';
  $('repair').hidden = !v.paired_name || showPairing;
  $('version').textContent = `Annex ${v.version}`;
}

async function refresh() {
  try {
    render(await invoke('status'));
  } catch (e) {
    $('message').textContent = String(e);
  }
}

$('monitors').addEventListener('change', async (e) => {
  const row = e.target.closest('.monitor');
  if (!row) return;
  const k = e.target.dataset.k;
  e.target.blur();
  if (k === 'match') {
    await invoke('set_mirror_match', { id: row.dataset.id, on: e.target.value === 'match' });
  } else {
    const args = { id: row.dataset.id, enabled: null, mode: null, source: null };
    args[k] = k === 'enabled' ? e.target.checked : e.target.value;
    await invoke('set_monitor', args);
  }
  setTimeout(refresh, 300);
});

$('pair-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const pin = $('pin').value.replace(/\D/g, '');
  if (pin.length !== 6) {
    $('pair-result').innerHTML = '<span class="bad">The PIN has 6 digits.</span>';
    return;
  }
  const pick = document.querySelector('input[name="pc"]:checked');
  await invoke('pair', { receiverId: pick ? pick.value : null, pin });
  $('pin').value = '';
  refresh();
});

$('cursor').addEventListener('change', (e) => invoke('set_client_cursor', { on: e.target.checked }));
$('autostart').addEventListener('change', (e) =>
  invoke('set_autostart', { on: e.target.checked }).catch((err) => {
    $('message').textContent = `Could not change the login item: ${err}`;
  }));
$('perm-open').addEventListener('click', () => invoke('open_screen_recording_settings'));
// The web view has no confirm() dialog, so Forget asks inline.
function askForget(open) {
  $('forget').hidden = open;
  $('forget-confirm').hidden = !open;
}
$('forget').addEventListener('click', () => askForget(true));
$('forget-no').addEventListener('click', () => askForget(false));
$('forget-yes').addEventListener('click', async () => {
  askForget(false);
  await invoke('forget');
  refresh();
});
$('repair').addEventListener('click', () => {
  showPairing = true;
  if (current) render(current);
});

refresh();
setInterval(refresh, 1000);
