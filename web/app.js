'use strict';

const $ = (id) => document.getElementById(id);
const state = { level: 'medium', maxBytes: 0, config: null, files: [], busy: false };
let nextKey = 1;

class ApiError extends Error {
  constructor(code, message, status) {
    super(message);
    this.code = code;
    this.status = status;
  }
}

const networkError = () =>
  new ApiError('NETWORK_ERROR', "Can't reach the Minifold server. Check that it's running and try again.", 0);

async function errorFrom(res) {
  try {
    const body = await res.json();
    return new ApiError(body.code, body.message, res.status);
  } catch {
    return new ApiError(`HTTP_${res.status}`, `The server answered with an unexpected error (${res.status}).`, res.status);
  }
}

async function request(path, options = {}) {
  let res;
  try {
    res = await fetch(path, { credentials: 'same-origin', ...options });
  } catch {
    throw networkError();
  }
  if (!res.ok) throw await errorFrom(res);
  return res;
}

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

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

const LEVEL_DESC = {
  low: 'Fastest, visually lossless.',
  medium: 'Balanced size and quality.',
  high: 'Much smaller files with slight quality loss.',
  extra: 'Smallest files; videos are capped at 720p.',
};
const LEVEL_NAME = { low: 'Low', medium: 'Medium', high: 'High', extra: 'Extra' };

// ---- views ----

function showLogin(message) {
  $('app-view').hidden = true;
  $('logout').hidden = true;
  $('login-view').hidden = false;
  $('login-error').hidden = !message;
  $('login-error').textContent = message || '';
  $('token').focus();
}

function showApp() {
  $('login-view').hidden = true;
  $('app-view').hidden = false;
  $('logout').hidden = !state.config.sign_in;
  $('drop-hint').textContent =
    `Any file up to ${human(state.maxBytes)} each. JPEG, PNG, WebP, GIF, HEIC, AVIF and TIFF keep their format, ` +
    'BMP becomes PNG, PDFs and videos (MP4, MOV, MKV, WebM) keep their format, and other files are compressed with zstd (.zst).';
  $('policy').textContent = policyText(state.config);
  render();
}

function duration(minutes) {
  if (minutes % 60 !== 0) return `${minutes} minute${minutes === 1 ? '' : 's'}`;
  const hours = minutes / 60;
  return `${hours} hour${hours === 1 ? '' : 's'}`;
}

function policyText(c) {
  const originals = c.delete_originals_after_job
    ? 'Originals are deleted right after compression.'
    : `Originals are deleted after ${duration(c.original_retention_minutes)}.`;
  const outputs = c.delete_after_download
    ? `Compressed files are deleted after the first download, or after ${duration(c.output_retention_minutes)} at the latest.`
    : `Compressed files are deleted after ${duration(c.output_retention_minutes)}.`;
  const metadata = c.strip_metadata ? ' Location, camera and author details are removed from images and PDFs.' : '';
  return `Everything is processed on this server. Nothing is sent anywhere else.${metadata} ${originals} ${outputs}`;
}

async function start() {
  try {
    const res = await request('/api/config');
    state.config = await res.json();
    state.maxBytes = state.config.max_upload_bytes;
    showApp();
  } catch (err) {
    showLogin(err.status === 401 ? '' : err.message);
  }
}

// ---- adding files ----

function reject(name, code, message) {
  const box = el('div', 'alert');
  box.setAttribute('role', 'alert');
  const text = el('div');
  text.appendChild(el('b', '', `${name} wasn't added`));
  const detail = el('span', '', `${message} `);
  detail.appendChild(el('span', 'code', code));
  text.appendChild(detail);
  const dismiss = el('button', 'icon-btn small');
  dismiss.type = 'button';
  dismiss.setAttribute('aria-label', 'Dismiss');
  dismiss.appendChild(icon('close', 20));
  dismiss.addEventListener('click', () => box.remove());
  box.append(text, dismiss);
  $('rejects').appendChild(box);
}

function addFiles(list) {
  for (const file of list) {
    if (file.size > state.maxBytes) {
      reject(file.name, 'FILE_TOO_LARGE', `It's ${human(file.size)} and the limit on this server is ${human(state.maxBytes)}.`);
    } else if (file.size === 0) {
      reject(file.name, 'EMPTY_FILE', "It's empty (or a folder), so there's nothing to compress.");
    } else {
      state.files.push({ key: nextKey++, file, status: 'ready', progress: 0, job: null, error: null, level: null, fast: false });
    }
  }
  render();
}

// ---- compressing ----

function upload(item) {
  return new Promise((resolve) => {
    const xhr = new XMLHttpRequest();
    const method = item.fast ? '&method=fast' : '';
    const url = `/api/jobs?name=${encodeURIComponent(item.file.name)}&level=${item.level}${method}`;
    xhr.open('POST', url);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) {
        item.progress = e.loaded / e.total;
        render();
      }
    };
    xhr.onload = () => {
      let body = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        body = null;
      }
      if (xhr.status === 202 && body) {
        item.job = body;
        setStatus(item, body.state);
      } else if (body && body.code) {
        fail(item, new ApiError(body.code, body.message, xhr.status));
      } else {
        fail(item, new ApiError(`HTTP_${xhr.status}`, `The server answered with an unexpected error (${xhr.status}).`, xhr.status));
      }
      resolve();
    };
    xhr.onerror = () => {
      fail(item, networkError());
      resolve();
    };
    item.status = 'uploading';
    render();
    xhr.send(item.file);
  });
}

function fail(item, err) {
  item.status = 'failed';
  item.error = { code: err.code, message: err.message };
  if (err.status === 401) showLogin('Your session ended. Sign in again to continue.');
}

/** Also starts the elapsed-time clock when compression begins. */
function setStatus(item, status) {
  if (status === 'running' && item.status !== 'running') item.runningSince = Date.now();
  item.status = status;
}

const elapsed = (item) => Math.round((Date.now() - (item.runningSince ?? Date.now())) / 1000);

function clock(seconds) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = String(seconds % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const inProgress = (item) => item.status === 'queued' || item.status === 'running';

async function poll(item) {
  const id = item.job.id;
  // Stops when the file was aborted or retried (a retry gets a new job id) meanwhile.
  const current = () => inProgress(item) && item.job?.id === id;
  while (current()) {
    await sleep(1000);
    if (!current()) return;
    try {
      const res = await request(`/api/jobs/${id}`);
      const latest = await res.json();
      if (!current()) return;
      item.job = latest;
      setStatus(item, latest.state);
      if (item.status === 'failed') item.error = latest.error;
    } catch (err) {
      if (!current()) return;
      fail(item, err.code === 'NETWORK_ERROR'
        ? new ApiError(err.code, 'Lost contact with the server while waiting. Reload the page to check again.', 0)
        : err);
    }
    render();
  }
}

/** Checks the file still exists first, so an expired file shows a message instead of a raw error page. */
async function download(item, href) {
  try {
    await request(`/api/jobs/${item.job.id}`);
  } catch (err) {
    if (err.status === 401) {
      showLogin('Your session ended. Sign in again to continue.');
      return;
    }
    item.status = 'gone';
    item.error = { code: err.code, message: err.message };
    render();
    return;
  }
  window.location.href = href;
  if (state.config.delete_after_download) {
    item.status = 'downloaded';
    render();
  }
}

/** Stops a queued or running file on the server; its files there are deleted right away. */
async function abort(item) {
  item.aborting = true;
  render();
  try {
    await request(`/api/jobs/${item.job.id}`, { method: 'DELETE' });
    item.abortedAfter = item.status === 'running' ? elapsed(item) : null;
    item.status = 'aborted';
    item.retryChoice = item.job.fast_available && !item.fast ? 'fast' : item.level;
  } catch (err) {
    if (err.status === 401) showLogin('Your session ended. Sign in again to continue.');
    // Otherwise it finished or failed meanwhile: the next poll shows that.
  }
  item.aborting = false;
  render();
}

/** Sends the file again from this tab, with the level or faster method picked after aborting. */
async function retry(item) {
  item.fast = item.retryChoice === 'fast';
  if (!item.fast) item.level = item.retryChoice;
  item.job = null;
  item.error = null;
  item.progress = 0;
  item.runningSince = null;
  await upload(item);
  render();
  if (inProgress(item)) await poll(item);
  render();
}

async function compressAll() {
  state.busy = true;
  render();
  const polls = [];
  for (const item of state.files.filter((f) => f.status === 'ready')) {
    item.level = state.level;
    await upload(item);
    if (item.status === 'queued' || item.status === 'running') polls.push(poll(item));
    render();
  }
  await Promise.all(polls);
  state.busy = false;
  render();
}

// ---- rendering ----

function extension(name) {
  const dot = name.lastIndexOf('.');
  const ext = dot > 0 ? name.slice(dot + 1) : '';
  return ext && ext.length <= 4 ? ext.toUpperCase() : 'FILE';
}

const STATUS_TEXT = {
  ready: 'Ready',
  uploading: 'Uploading',
  queued: 'Waiting in queue',
  running: 'Compressing…',
  aborted: 'Aborted',
  done: 'Done',
  failed: 'Failed',
  gone: 'Deleted',
  downloaded: 'Downloaded',
};

function messageBox(className, title, message, code) {
  const box = el('div', className);
  box.appendChild(el('b', '', title));
  const line = el('span', '', `${message} `);
  if (code) line.appendChild(el('span', 'code', code));
  box.appendChild(line);
  return box;
}

function fileRow(item) {
  // An aborted row is kept as is, so re-renders for other files don't close its open dropdown.
  if (item.status === 'aborted' && item.row) return item.row;
  const li = el('li', `file ${item.status}`);
  item.row = item.status === 'aborted' ? li : null;
  li.appendChild(el('span', 'badge-type', extension(item.file.name)));

  const main = el('div', 'file-main');
  main.appendChild(el('div', 'file-name', item.file.name));
  const note = item.status === 'done' && !item.job.no_gain ? ` · ${item.job.note}` : '';
  const aborted = item.abortedAfter == null ? '' : ` after ${clock(item.abortedAfter)}`;
  const status = item.status === 'aborted' ? `Aborted${aborted} · deleted from the server` : STATUS_TEXT[item.status];
  main.appendChild(el('div', 'file-sub', `${human(item.file.size)} · ${status}${note}`));
  li.appendChild(main);

  const encoding = item.status === 'running' && item.job.progress > 0;
  if (item.status === 'running' || item.status === 'queued') {
    if (item.status === 'running') {
      const bar = el('div', encoding ? 'progress' : 'progress indeterminate');
      bar.setAttribute('role', 'progressbar');
      bar.setAttribute('aria-label', `Compressing ${item.file.name}`);
      const fill = el('span');
      if (encoding) {
        bar.setAttribute('aria-valuenow', String(item.job.progress));
        fill.style.width = `${item.job.progress}%`;
      }
      bar.appendChild(fill);
      const time = clock(elapsed(item));
      li.append(bar, el('span', 'run-meta', encoding ? `${item.job.progress}% · ${time}` : time));
    }
    const stop = el('button', 'btn outlined abort');
    stop.type = 'button';
    stop.disabled = !!item.aborting;
    stop.setAttribute('aria-label', `Abort ${item.file.name}`);
    stop.append(icon('close', 18), 'Abort');
    stop.addEventListener('click', () => abort(item));
    li.appendChild(stop);
  } else if (item.status === 'aborted') {
    li.appendChild(retryControls(item));
  } else if (item.status === 'uploading') {
    const pct = Math.round(item.progress * 100);
    const bar = el('div', 'progress');
    bar.setAttribute('role', 'progressbar');
    bar.setAttribute('aria-label', `Uploading ${item.file.name}`);
    bar.setAttribute('aria-valuenow', String(pct));
    const fill = el('span');
    fill.style.width = `${pct}%`;
    bar.appendChild(fill);
    li.appendChild(bar);
  } else if (item.status === 'failed') {
    li.appendChild(messageBox('file-error', "Couldn't compress this file", item.error.message, item.error.code));
  } else if (item.status === 'gone') {
    li.appendChild(messageBox('file-info', 'No longer on the server', item.error.message, item.error.code));
  } else if (item.status === 'downloaded') {
    li.appendChild(messageBox('file-info', 'Downloaded', 'It was deleted from the server after the download.', ''));
  } else if (item.status === 'done') {
    const job = item.job;
    if (job.no_gain) {
      li.appendChild(messageBox('file-info', 'Kept as is', job.note, 'NO_GAIN'));
    } else {
      const detail = el('div', 'file-detail');
      const pct = Math.round((1 - job.output_size / job.original_size) * 100);
      detail.append(el('span', 'muted', human(job.original_size)), el('span', 'muted', '→'), el('b', '', human(job.output_size)), el('span', 'saved', `−${pct}%`));
      li.appendChild(detail);
    }
    const link = el('a', 'btn tonal download');
    link.append(icon('download', 18), 'Download');
    link.href = `/api/jobs/${job.id}/download`;
    link.addEventListener('click', (e) => {
      e.preventDefault();
      download(item, link.href);
    });
    li.appendChild(link);
  } else if (item.status === 'ready' && !state.busy) {
    const remove = el('button', 'icon-btn small');
    remove.type = 'button';
    remove.setAttribute('aria-label', `Remove ${item.file.name}`);
    remove.appendChild(icon('close', 20));
    remove.addEventListener('click', () => {
      state.files = state.files.filter((f) => f !== item);
      render();
    });
    li.appendChild(remove);
  }
  return li;
}

function retryControls(item) {
  const box = el('div', 'retry');
  const field = el('label', 'field select');
  const select = el('select');
  if (item.job.fast_available) {
    const fast = el('option', '', `Faster: ${LEVEL_NAME[item.level]} level, quicker encoder, slightly larger file`);
    fast.value = 'fast';
    select.appendChild(fast);
  }
  for (const [level, name] of Object.entries(LEVEL_NAME)) {
    const aborted = level === item.level && !item.fast ? ' (aborted)' : '';
    const option = el('option', '', `${name} level: ${LEVEL_DESC[level]}${aborted}`);
    option.value = level;
    select.appendChild(option);
  }
  select.value = item.retryChoice;
  select.addEventListener('change', () => {
    item.retryChoice = select.value;
  });
  field.append(select, el('span', '', 'Retry with'));

  const go = el('button', 'btn tonal');
  go.type = 'button';
  go.append(icon('restart_alt', 18), 'Retry');
  go.addEventListener('click', () => retry(item));

  const remove = el('button', 'icon-btn small');
  remove.type = 'button';
  remove.setAttribute('aria-label', `Remove ${item.file.name}`);
  remove.appendChild(icon('close', 20));
  remove.addEventListener('click', () => {
    state.files = state.files.filter((f) => f !== item);
    render();
  });
  box.append(field, go, remove);
  return box;
}

function totals() {
  const done = state.files.filter((f) => f.status === 'done');
  const failed = state.files.filter((f) => f.status === 'failed').length;
  const compressed = done.filter((f) => !f.job.no_gain);
  const kept = done.length - compressed.length;
  if (done.length === 0 && failed === 0) return '';
  const before = compressed.reduce((sum, f) => sum + f.job.original_size, 0);
  const after = compressed.reduce((sum, f) => sum + f.job.output_size, 0);
  const parts = [`${compressed.length} compressed`];
  if (kept) parts.push(`${kept} kept`);
  if (failed) parts.push(`${failed} failed`);
  if (before > 0) parts.push(`saved ${human(before - after)} (−${Math.round((1 - after / before) * 100)}%)`);
  return parts.join(' · ');
}

/** Like replaceChildren, but rows that stay in place aren't detached. */
function placeRows(list, rows) {
  const keep = new Set(rows);
  for (const child of [...list.children]) if (!keep.has(child)) child.remove();
  rows.forEach((row, i) => {
    if (list.children[i] !== row) list.insertBefore(row, list.children[i] ?? null);
  });
}

function render() {
  placeRows($('files'), state.files.map(fileRow));
  const ready = state.files.filter((f) => f.status === 'ready').length;
  $('count').textContent = `(${state.files.length})`;
  $('empty').hidden = state.files.length > 0;
  $('totals').textContent = totals();
  $('compress').disabled = state.busy || ready === 0;
  $('compress-label').textContent = ready > 0 ? `Compress ${ready} file${ready === 1 ? '' : 's'}` : 'Compress';
  $('level-desc').textContent = LEVEL_DESC[state.level];
  $('clear').hidden = state.busy || state.files.length === 0;
  for (const button of document.querySelectorAll('.level')) {
    button.setAttribute('aria-pressed', String(button.dataset.level === state.level));
    button.disabled = state.busy;
  }
}

// ---- wiring ----

document.addEventListener('DOMContentLoaded', () => {
  $('login-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      await request('/api/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token: $('token').value }),
      });
      $('token').value = '';
      await start();
    } catch (err) {
      showLogin(err.message);
    }
  });

  $('logout').addEventListener('click', async () => {
    try {
      await request('/api/logout', { method: 'POST' });
    } finally {
      showLogin();
    }
  });

  for (const button of document.querySelectorAll('.level')) {
    button.addEventListener('click', () => {
      state.level = button.dataset.level;
      render();
    });
  }

  $('picker').addEventListener('change', (e) => {
    addFiles(e.target.files);
    e.target.value = '';
  });

  const drop = $('drop');
  window.addEventListener('dragover', (e) => e.preventDefault());
  window.addEventListener('drop', (e) => e.preventDefault());
  drop.addEventListener('dragover', () => drop.classList.add('over'));
  drop.addEventListener('dragleave', () => drop.classList.remove('over'));
  drop.addEventListener('drop', (e) => {
    drop.classList.remove('over');
    if (!state.busy) addFiles(e.dataTransfer.files);
  });

  $('compress').addEventListener('click', compressAll);
  $('clear').addEventListener('click', () => {
    state.files = [];
    $('rejects').replaceChildren();
    render();
  });

  start();
});
