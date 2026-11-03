'use strict';

const $ = (id) => document.getElementById(id);
const NUMBER_FIELDS = ['max_upload_gb', 'workers', 'original_retention_minutes', 'output_retention_minutes'];
const BOOL_FIELDS = ['delete_originals_after_job', 'delete_after_download', 'strip_metadata'];
const RESTART_TIMEOUT_MS = 60000;
const HW_LABELS = { off: 'Off (CPU only)', nvenc: 'NVIDIA NVENC', qsv: 'Intel Quick Sync', vaapi: 'VA-API (Intel/AMD)' };
let saved = null;

function human(bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
}

async function request(path, options = {}) {
  let res;
  try {
    res = await fetch(path, { credentials: 'same-origin', ...options });
  } catch {
    throw Object.assign(new Error("Can't reach the Minifold server. Check that it's running."), { code: 'NETWORK_ERROR', status: 0 });
  }
  if (res.ok) return res;
  let body = null;
  try {
    body = await res.json();
  } catch {
    body = null;
  }
  const message = body ? body.message : `The server answered with an unexpected error (${res.status}).`;
  throw Object.assign(new Error(message), { code: body ? body.code : `HTTP_${res.status}`, status: res.status });
}

// ---- views ----

function showLogin(message) {
  $('admin-view').hidden = true;
  $('logout').hidden = true;
  $('login-view').hidden = false;
  $('login-error').hidden = !message;
  $('login-error').textContent = message || '';
  $('token').focus();
}

function showError(err) {
  if (err.status === 401) {
    showLogin('Your admin session ended. Sign in again.');
    return;
  }
  const box = $('error');
  box.replaceChildren();
  const b = document.createElement('b');
  b.textContent = err.code === 'INVALID_SETTING' ? "Settings weren't saved" : 'Something went wrong';
  const span = document.createElement('span');
  span.textContent = `${err.message} `;
  const code = document.createElement('span');
  code.className = 'code';
  code.textContent = err.code;
  span.appendChild(code);
  const text = document.createElement('div');
  text.append(b, span);
  box.appendChild(text);
  box.hidden = false;
  box.scrollIntoView({ block: 'nearest' });
}

function showNotice(message) {
  $('notice').textContent = message;
  $('notice').hidden = !message;
}

function render(view) {
  saved = view.saved;
  for (const f of NUMBER_FIELDS) $(f).value = String(view.saved[f]);
  for (const f of BOOL_FIELDS) $(f).checked = view.saved[f];
  renderHw(view);
  syncDependent();

  const s = view.system;
  $('disk-hint').textContent = `Per file. Free space on the data disk: ${human(s.data_free_bytes)}. Keep at least 2.5× the limit free.`;
  $('cpu-hint').textContent = `${s.cpus} CPU cores detected. Other files wait in the queue.`;
  const rows = [
    ['Version', s.version],
    ['Architecture', s.arch],
    ['CPU cores', String(s.cpus)],
    ['Free space (data)', human(s.data_free_bytes)],
  ];
  $('system').replaceChildren(...rows.flatMap(([k, v]) => {
    const dt = document.createElement('dt');
    dt.textContent = k;
    const dd = document.createElement('dd');
    dd.textContent = v;
    return [dt, dd];
  }));

  $('restart-bar').hidden = !view.restart_required;
  const jobs = view.jobs_in_progress;
  $('restart-detail').textContent =
    (jobs > 0 ? `${jobs} job${jobs === 1 ? ' is' : 's are'} in progress and will be cancelled. ` : 'No jobs are running. ') +
    'Minifold stops cleanly and Docker starts it again (the container needs the restart policy "unless-stopped").';
  $('save').disabled = true;
}

/** Only detected accelerators are offered; a selected-but-missing one gets a warning instead. */
function renderHw(view) {
  $('hw-section').hidden = view.hw_detected.length === 0 && !view.hw_missing;
  const legend = document.createElement('legend');
  legend.className = 'sr-only';
  legend.textContent = 'Video encoder';
  const choices = ['off', ...view.hw_detected].map((value) => {
    const label = document.createElement('label');
    label.className = 'choice';
    const input = document.createElement('input');
    input.type = 'radio';
    input.name = 'hw_accel';
    input.value = value;
    input.checked = view.saved.hw_accel === value;
    const text = document.createElement('span');
    text.textContent = HW_LABELS[value];
    label.append(input, text);
    return label;
  });
  $('hw-choices').replaceChildren(legend, ...choices);
  $('hw-warning').hidden = !view.hw_missing;
  $('hw-warning').textContent = view.hw_missing
    ? `${HW_LABELS[view.hw_missing]} is selected, but it wasn't found when Minifold started. Video jobs fail (HW_UNAVAILABLE) until you choose another option under Hardware acceleration, save and restart.`
    : '';
}

function current() {
  const out = {};
  for (const f of NUMBER_FIELDS) out[f] = Number($(f).value);
  for (const f of BOOL_FIELDS) out[f] = $(f).checked;
  const hw = document.querySelector('input[name=hw_accel]:checked');
  out.hw_accel = hw ? hw.value : saved.hw_accel;
  return out;
}

function syncDependent() {
  $('original-retention-row').hidden = $('delete_originals_after_job').checked;
}

function onEdit() {
  syncDependent();
  $('error').hidden = true;
  showNotice('');
  const now = current();
  $('save').disabled = Object.keys(now).every((k) => now[k] === saved[k]);
}

/** Client-side check so obvious mistakes are explained before saving; the server checks again. */
function invalidField() {
  for (const f of NUMBER_FIELDS) {
    const input = $(f);
    if (input.closest('[hidden]')) continue;
    if (!input.checkValidity() || !Number.isInteger(Number(input.value))) {
      const label = input.closest('.setting').querySelector('b').textContent;
      return { input, message: `${label} must be a whole number between ${input.min} and ${input.max}.` };
    }
  }
  return null;
}

async function load() {
  try {
    const res = await request('/api/admin/settings');
    $('login-view').hidden = true;
    $('admin-view').hidden = false;
    $('logout').hidden = false;
    render(await res.json());
  } catch (err) {
    if (err.status === 401) showLogin();
    else showLogin(err.message);
  }
}

async function save(e) {
  e.preventDefault();
  const problem = invalidField();
  if (problem) {
    showError({ code: 'INVALID_SETTING', message: problem.message });
    problem.input.focus();
    return;
  }
  $('save').disabled = true;
  try {
    const res = await request('/api/admin/settings', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(current()),
    });
    render(await res.json());
    $('restart-bar').scrollIntoView({ block: 'nearest' });
  } catch (err) {
    $('save').disabled = false;
    showError(err);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function restart() {
  $('restart').disabled = true;
  try {
    await request('/api/admin/restart', { method: 'POST' });
  } catch (err) {
    $('restart').disabled = false;
    showError(err);
    return;
  }
  $('restart-bar').hidden = true;
  showNotice('Restarting… this page reloads when Minifold is back.');
  await sleep(1500);
  const deadline = Date.now() + RESTART_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const res = await fetch('/healthz', { cache: 'no-store' });
      if (res.ok) {
        location.replace('/admin?restarted=1');
        return;
      }
    } catch {
      // still down
    }
    await sleep(1000);
  }
  showNotice('');
  $('restart').disabled = false;
  showError({
    code: 'RESTART_TIMEOUT',
    message: "Minifold didn't come back within 60 seconds. Make sure the container's restart policy is \"unless-stopped\", or start the container yourself.",
  });
}

document.addEventListener('DOMContentLoaded', () => {
  $('login-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      await request('/api/admin/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token: $('token').value }),
      });
      $('token').value = '';
      await load();
    } catch (err) {
      showLogin(err.message);
    }
  });
  $('logout').addEventListener('click', async () => {
    try {
      await request('/api/admin/logout', { method: 'POST' });
    } finally {
      showLogin();
    }
  });
  $('settings-form').addEventListener('input', onEdit);
  $('settings-form').addEventListener('submit', save);
  $('restart').addEventListener('click', restart);

  if (new URLSearchParams(location.search).get('restarted') === '1') {
    showNotice('Minifold restarted. The new settings are active.');
    history.replaceState(null, '', '/admin');
  }
  load();
});
