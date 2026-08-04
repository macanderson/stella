/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright (c) 2026 the ArenaBench authors
 *
 * The whole client. No framework and no build step, which for a benchmark tool
 * is a feature: `pip install arenabench && arenabench serve` is the entire
 * setup, and anyone can read what the dashboard is doing to their numbers.
 *
 * Two live channels, both SSE:
 *   /api/matches/<id>/stream                          -> scoreboard snapshots
 *   /api/matches/<id>/transcript/<contestant>/<task>   -> one trial, streaming
 *
 * Transcript entries carry a `seq`, and a streaming entry is re-sent under the
 * same `seq` as it grows. Keying rendered nodes by seq therefore gives
 * append-or-replace for free, with no diffing and no reflow of the backlog.
 */

'use strict';

// ------------------------------------------------------------------ state

const State = {
  datasets: [],
  agents: [],
  efforts: [],
  roles: [],
  dataset: null,
  tasks: [],
  selected: new Set(),
  draw: null,           // the last random draw, while the selection still is it
  seats: [],
  match: null,
  snapshot: null,
  activeTask: null,
  streams: new Map(),   // key -> EventSource
  scoreStream: null,
  follow: true,
  showReasoning: true,
  seatCounter: 0,
};

const PALETTE = ['#FFB000', '#37D5F2', '#C77DFF', '#5CE68A', '#FF6B9D', '#FFE066', '#7AA2F7', '#FF8C42'];

const $  = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

function el(tag, attrs = {}, ...kids) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value == null || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'style') node.setAttribute('style', value);
    else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else if (key === 'html') node.innerHTML = value;
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    node.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return node;
}

// ------------------------------------------------------------ formatting

const fmt = {
  tokens(n) {
    n = Number(n) || 0;
    if (n >= 1e9) return (n / 1e9).toFixed(2) + 'B';
    if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return String(Math.round(n));
  },
  money(n) {
    n = Number(n) || 0;
    return n >= 100 ? '$' + n.toFixed(0) : n >= 1 ? '$' + n.toFixed(2) : '$' + n.toFixed(4);
  },
  clock(seconds) {
    seconds = Math.max(0, Math.round(Number(seconds) || 0));
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    return h ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
             : `${m}:${String(s).padStart(2, '0')}`;
  },
  pct(n) { return (Number(n) || 0).toFixed(0) + '%'; },
  dim(key, value) {
    switch (key) {
      case 'solve_rate':  return fmt.pct(value);
      case 'clock_time':  return fmt.clock(value);
      case 'total_cost':  return fmt.money(value);
      default:            return fmt.tokens(value);
    }
  },
};

async function api(path, options) {
  const response = await fetch(path, options);
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
  return body;
}

function setConn(state, label) {
  const dot = $('#conn-dot');
  dot.className = 'status-dot' + (state ? ` is-${state}` : '');
  $('#conn-label').textContent = label;
}

// ==================================================================== setup

async function boot() {
  wireChrome();
  try {
    const [datasets, agents] = await Promise.all([api('/api/datasets'), api('/api/agents')]);
    State.datasets = datasets.datasets;
    State.agents = agents.agents;
    State.efforts = agents.efforts;
    State.roles = agents.roles;
    renderDatasets();
    addSeat('stella');
    addSeat('claude-code');
    refreshHistory();
  } catch (error) {
    $('#dataset-grid').replaceChildren(el('div', { class: 'errors' }, String(error)));
  }
}

function wireChrome() {
  $$('.tab').forEach((tab) => tab.addEventListener('click', () => showView(tab.dataset.view)));
  $('#add-contestant').addEventListener('click', () => addSeat('stella'));
  $('#launch').addEventListener('click', launch);
  $('#cancel-match').addEventListener('click', cancelMatch);
  $('#select-all').addEventListener('click', () => { State.tasks.forEach((t) => State.selected.add(t.name)); renderTasks(); });
  $('#select-none').addEventListener('click', () => { State.selected.clear(); renderTasks(); });
  $('#select-visible').addEventListener('click', () => { visibleTasks().forEach((t) => State.selected.add(t.name)); renderTasks(); });
  $('#select-random').addEventListener('click', drawRandomTasks);
  ['#task-search', '#filter-difficulty', '#filter-category', '#filter-light']
    .forEach((sel) => $(sel).addEventListener('input', renderTasks));
  $('#toggle-reasoning').addEventListener('click', () => {
    State.showReasoning = !State.showReasoning;
    $('#toggle-reasoning').textContent = State.showReasoning ? 'hide reasoning' : 'show reasoning';
    $$('.feed').forEach((feed) => feed.classList.toggle('hide-reasoning', !State.showReasoning));
  });
  $('#toggle-follow').addEventListener('click', () => {
    State.follow = !State.follow;
    $('#toggle-follow').textContent = State.follow ? 'following' : 'paused';
  });
}

function showView(name) {
  $$('.tab').forEach((tab) => tab.classList.toggle('is-active', tab.dataset.view === name));
  $$('.view').forEach((view) => view.classList.toggle('is-active', view.id === `view-${name}`));
}

// -- datasets --------------------------------------------------------------

function renderDatasets() {
  const grid = $('#dataset-grid');
  grid.replaceChildren(...State.datasets.map((dataset) =>
    el('div', {
      class: 'dataset-card' + (State.dataset?.key === dataset.key ? ' is-selected' : ''),
      onclick: () => pickDataset(dataset),
    },
      el('h3', {}, dataset.title),
      el('p', {}, dataset.description),
      el('div', { class: 'dataset-meta' },
        el('span', {}, dataset.task_count != null ? `${dataset.task_count} tasks` : 'not fetched'),
        el('span', { class: 'dataset-digest' }, dataset.digest ? dataset.digest.slice(0, 19) + '…' : 'unpinned'),
      ),
    )));
}

async function pickDataset(dataset) {
  State.dataset = dataset;
  State.selected.clear();
  renderDatasets();
  $('#step-tasks').hidden = false;
  $('#task-grid').replaceChildren(el('div', { class: 'loading' }, 'loading tasks…'));
  try {
    const payload = await api(`/api/datasets/${encodeURIComponent(dataset.key)}/tasks`);
    State.tasks = payload.tasks;
    if (!State.tasks.length) {
      $('#task-grid').replaceChildren(el('div', { class: 'errors' }, payload.hint || 'no tasks found'));
      return;
    }
    fillFilters();
    renderTasks();
    $('#step-contestants').hidden = false;
    $('#step-launch').hidden = false;
  } catch (error) {
    $('#task-grid').replaceChildren(el('div', { class: 'errors' }, String(error)));
  }
}

function fillFilters() {
  const uniq = (key) => [...new Set(State.tasks.map((t) => t[key]).filter(Boolean))].sort();
  const fill = (sel, values, label) => {
    $(sel).replaceChildren(
      el('option', { value: '' }, label),
      ...values.map((v) => el('option', { value: v }, v)),
    );
  };
  fill('#filter-difficulty', uniq('difficulty'), 'all difficulty');
  fill('#filter-category', uniq('category'), 'all categories');
}

// The draw happens on the server, not here. `Math.random` cannot be seeded,
// and an unseeded slice is a number nobody — including you tomorrow — can
// reproduce. The seed comes back and is shown, so the selection can be
// written down and run again.
async function drawRandomTasks() {
  if (!State.dataset) return;
  const count = Math.max(1, Number($('#random-n').value) || 10);
  const heavy = $('#filter-light').checked ? '1' : '0';
  const maxMem = Math.max(0, Number($('#random-max-mem').value) || 0);
  const seedEl = $('#random-seed');
  try {
    const drawn = await api(
      `/api/datasets/${encodeURIComponent(State.dataset.key)}/sample` +
      `?count=${count}&exclude_heavy=${heavy}&max_memory_mb=${maxMem}`);
    State.selected = new Set(drawn.names);
    State.draw = new Set(drawn.names);
    seedEl.textContent =
      `· ${drawn.names.length} drawn, seed ${drawn.seed}` +
      (drawn.exclude_heavy ? ', heavy excluded' : '') +
      (drawn.max_memory_mb ? `, ≤${drawn.max_memory_mb} MB` : '');
    renderTasks();
  } catch (err) {
    seedEl.textContent = `· draw failed: ${err.message}`;
  }
}

function visibleTasks() {
  const q = $('#task-search').value.trim().toLowerCase();
  const difficulty = $('#filter-difficulty').value;
  const category = $('#filter-category').value;
  const lightOnly = $('#filter-light').checked;
  return State.tasks.filter((task) => {
    if (difficulty && task.difficulty !== difficulty) return false;
    if (category && task.category !== category) return false;
    if (lightOnly && task.heavy) return false;
    if (q && !(task.name + ' ' + task.description).toLowerCase().includes(q)) return false;
    return true;
  });
}

function renderTasks() {
  const tasks = visibleTasks();
  // The seed describes a draw. The moment the selection stops being that
  // draw — one checkbox toggled, "select all" pressed — the seed no longer
  // reproduces what is about to run, so it stops being shown.
  if (State.draw) {
    const intact = State.draw.size === State.selected.size
      && [...State.draw].every((name) => State.selected.has(name));
    if (!intact) { State.draw = null; $('#random-seed').textContent = ''; }
  }
  $('#task-count').textContent = String(State.selected.size);
  $('#task-grid').replaceChildren(...tasks.map((task) => {
    const on = State.selected.has(task.name);
    return el('label', { class: 'task-chip' + (on ? ' is-on' : '') },
      el('input', {
        type: 'checkbox', checked: on,
        onchange: (event) => {
          if (event.target.checked) State.selected.add(task.name);
          else State.selected.delete(task.name);
          renderTasks();
          updateSummary();
        },
      }),
      el('div', { class: 'task-chip-body' },
        el('div', { class: 'task-chip-name', title: task.description }, task.name),
        el('div', { class: 'task-chip-meta' },
          task.difficulty && el('span', { class: `diff-${task.difficulty}` }, task.difficulty),
          task.category && el('span', {}, task.category),
          task.heavy && el('span', { class: 'tag-heavy' }, 'heavy'),
        ),
      ),
    );
  }));
  updateSummary();
}

// -- contestants -----------------------------------------------------------

function addSeat(agentSlug) {
  const index = State.seats.length;
  const spec = State.agents.find((a) => a.slug === agentSlug) || State.agents[0];
  State.seats.push({
    id: `seat-${++State.seatCounter}`,
    name: spec ? spec.title : `Seat ${index + 1}`,
    agent: spec ? spec.slug : 'stella',
    color: PALETTE[index % PALETTE.length],
    engine: { api: 'openrouter', model: '', reasoning: true, effort: 'high', base_url: '', budget_usd: '', max_tokens: '', roles: {} },
    env: '',
  });
  renderSeats();
}

function renderSeats() {
  $('#contestants').replaceChildren(...State.seats.map((seat, index) => seatNode(seat, index)));
  updateSummary();
}

function seatNode(seat, index) {
  const spec = State.agents.find((a) => a.slug === seat.agent);
  const honours = new Set(spec ? spec.honours : []);

  const bind = (obj, key, transform = (v) => v) => (event) => {
    obj[key] = transform(event.target.type === 'checkbox' ? event.target.checked : event.target.value);
    updateSummary();
  };

  const node = el('div', { class: 'seat', style: `--seat:${seat.color}` },
    el('div', { class: 'seat-head' },
      el('div', { class: 'seat-badge' }, String(index + 1)),
      el('div', { class: 'seat-name' },
        el('input', { type: 'text', value: seat.name, oninput: bind(seat, 'name') })),
      el('select', {
        onchange: (event) => { seat.agent = event.target.value; renderSeats(); },
      }, ...State.agents.map((a) => el('option', { value: a.slug, selected: a.slug === seat.agent }, a.title))),
      State.seats.length > 1 && el('button', {
        class: 'btn btn-ghost btn-sm btn-danger',
        onclick: () => { State.seats.splice(index, 1); renderSeats(); },
      }, 'remove'),
    ),

    el('div', { class: 'seat-grid' },
      el('label', { class: 'field' }, el('span', {}, 'API / provider'),
        el('input', { type: 'text', value: seat.engine.api, list: 'apis', oninput: bind(seat.engine, 'api') })),
      el('label', { class: 'field' }, el('span', {}, 'Model'),
        el('input', { type: 'text', value: seat.engine.model, placeholder: 'z-ai/glm-5.2', oninput: bind(seat.engine, 'model') })),
      el('label', { class: 'field field-sm' }, el('span', {}, 'Reasoning'),
        el('select', { disabled: !honours.has('reasoning'), onchange: bind(seat.engine, 'reasoning', (v) => v === 'on') },
          el('option', { value: 'on', selected: seat.engine.reasoning }, 'on'),
          el('option', { value: 'off', selected: !seat.engine.reasoning }, 'off'))),
      el('label', { class: 'field field-sm' }, el('span', {}, 'Effort'),
        el('select', { disabled: !honours.has('effort'), onchange: bind(seat.engine, 'effort') },
          ...State.efforts.map((e) => el('option', { value: e, selected: e === seat.engine.effort }, e)))),
    ),

    el('details', { class: 'seat-adv' },
      el('summary', {}, 'advanced — base URL, budget, output cap' + (spec?.has_pipeline ? ', pipeline roles' : '')),
      el('div', { class: 'seat-grid', style: 'margin-top:10px' },
        el('label', { class: 'field' }, el('span', {}, 'Base URL'),
          el('input', { type: 'text', value: seat.engine.base_url, placeholder: 'provider default', oninput: bind(seat.engine, 'base_url') })),
        el('label', { class: 'field field-sm' }, el('span', {}, 'Budget $/task'),
          el('input', { type: 'number', step: '0.01', value: seat.engine.budget_usd, placeholder: 'none', oninput: bind(seat.engine, 'budget_usd') })),
        el('label', { class: 'field field-sm' }, el('span', {}, 'Max output tok'),
          el('input', { type: 'number', step: '1000', value: seat.engine.max_tokens, placeholder: 'agent default', oninput: bind(seat.engine, 'max_tokens') })),
      ),
      spec?.has_pipeline && el('div', { class: 'roles' },
        el('div', { class: 'muted', style: 'font-size:11.5px;margin-top:6px' },
          'Per-role overrides. Blank inherits the engine baseline above.'),
        ...State.roles.map((role) => roleRow(seat, role)),
      ),
    ),

    el('div', { class: 'env-box' },
      el('label', { class: 'field' },
        el('span', {}, `.env for this seat`),
        el('textarea', {
          spellcheck: 'false',
          placeholder: 'OPENROUTER_API_KEY=sk-or-...\n# comments and `export` are fine',
          oninput: (event) => { seat.env = event.target.value; updateEnvStatus(seat, node); updateSummary(); },
        }, seat.env)),
      el('div', { class: 'env-status' }),
    ),
  );

  updateEnvStatus(seat, node);
  return node;
}

function roleRow(seat, role) {
  const current = seat.engine.roles[role] || (seat.engine.roles[role] = {});
  const set = (key) => (event) => { current[key] = event.target.value; updateSummary(); };
  return el('div', { class: 'role-row' },
    el('label', {}, role),
    el('input', { type: 'text', placeholder: 'inherit model', value: current.model || '', oninput: set('model') }),
    el('select', { onchange: set('effort') },
      el('option', { value: '' }, 'inherit'),
      ...State.efforts.map((e) => el('option', { value: e, selected: e === current.effort }, e))),
    el('select', { onchange: set('reasoning') },
      el('option', { value: '' }, 'inherit'),
      el('option', { value: 'on', selected: current.reasoning === 'on' }, 'on'),
      el('option', { value: 'off', selected: current.reasoning === 'off' }, 'off')),
    el('input', { type: 'number', placeholder: 'max tok', value: current.max_tokens || '', oninput: set('max_tokens') }),
  );
}

/* Parse the pasted env just enough to reassure the operator that the key they
   think they pasted is actually there. Mirrors the server's parser for the
   common cases; the server's result is authoritative. */
function parseEnvKeys(text) {
  const keys = [];
  for (let line of String(text).split('\n')) {
    line = line.trim();
    if (!line || line.startsWith('#')) continue;
    if (line.startsWith('export ')) line = line.slice(7).trim();
    const eq = line.indexOf('=');
    if (eq < 1) continue;
    const key = line.slice(0, eq).trim();
    if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) keys.push(key);
  }
  return keys;
}

const CREDENTIALS = {
  openrouter: ['OPENROUTER_API_KEY'], anthropic: ['ANTHROPIC_API_KEY'],
  openai: ['OPENAI_API_KEY'], zai: ['ZAI_API_KEY'],
  google: ['GEMINI_API_KEY', 'GOOGLE_API_KEY'], deepseek: ['DEEPSEEK_API_KEY'],
  moonshot: ['MOONSHOT_API_KEY'], xai: ['XAI_API_KEY'], mistral: ['MISTRAL_API_KEY'],
};

function updateEnvStatus(seat, node) {
  const status = $('.env-status', node);
  if (!status) return;
  const keys = parseEnvKeys(seat.env);
  const wanted = CREDENTIALS[(seat.engine.api || '').toLowerCase()] || [];
  const has = wanted.some((k) => keys.includes(k));
  if (!keys.length) {
    status.className = 'env-status env-warn';
    status.textContent = 'no variables — this seat has no credential';
  } else if (wanted.length && !has) {
    status.className = 'env-status env-warn';
    status.textContent = `${keys.length} vars, but none of: ${wanted.join(' / ')}`;
  } else {
    status.className = 'env-status env-ok';
    status.textContent = `${keys.length} vars · ${keys.slice(0, 6).join(' ')}${keys.length > 6 ? ' …' : ''}`;
  }
}

function updateSummary() {
  const tasks = State.selected.size;
  const seats = State.seats.length;
  $('#launch-summary').textContent =
    `${tasks} task${tasks === 1 ? '' : 's'} × ${seats} contestant${seats === 1 ? '' : 's'} = ${tasks * seats} trials`;
}

// -- launch ----------------------------------------------------------------

function seatPayload(seat) {
  const engine = { ...seat.engine };
  const roles = {};
  for (const [role, cfg] of Object.entries(seat.engine.roles || {})) {
    const entry = {};
    if (cfg.model) entry.model = cfg.model;
    if (cfg.effort) entry.effort = cfg.effort;
    if (cfg.reasoning) entry.reasoning = cfg.reasoning;
    if (cfg.max_tokens) entry.max_tokens = Number(cfg.max_tokens);
    if (Object.keys(entry).length) roles[role] = entry;
  }
  engine.roles = roles;
  return { id: seat.id, name: seat.name, agent: seat.agent, color: seat.color, engine, env: seat.env };
}

async function launch() {
  const errors = $('#launch-errors');
  errors.hidden = true;
  const payload = {
    name: $('#match-name').value.trim() || undefined,
    dataset: State.dataset?.key,
    tasks: [...State.selected],
    contestants: State.seats.map(seatPayload),
    attempts: Number($('#attempts').value) || 1,
    concurrency: Number($('#concurrency').value) || 1,
    setup_timeout_multiplier: Number($('#setup-timeout').value) || 1,
    record_video: $('#record-video').checked,
  };
  if (!payload.tasks.length) {
    errors.hidden = false;
    errors.textContent = 'Select at least one task.';
    return;
  }
  $('#launch').disabled = true;
  $('#launch').textContent = 'starting…';
  try {
    const snapshot = await api('/api/matches', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    openMatch(snapshot.match.id, snapshot);
  } catch (error) {
    errors.hidden = false;
    errors.textContent = String(error);
  } finally {
    $('#launch').disabled = false;
    $('#launch').textContent = '▶ Start the match';
  }
}

async function refreshHistory() {
  try {
    const { matches } = await api('/api/matches');
    const box = $('#history');
    if (!matches.length) { box.replaceChildren(el('span', { class: 'muted' }, 'none yet')); return; }
    box.replaceChildren(...matches.map((m) =>
      el('div', { class: 'history-row', onclick: () => openMatch(m.id) },
        el('span', { class: 'pill is-' + m.status }, m.status),
        el('strong', {}, m.name),
        el('span', { class: 'muted mono' }, `${m.tasks} tasks · ${m.contestants.join(' vs ')}`),
      )));
  } catch { /* history is a nicety; never block setup on it */ }
}

// ==================================================================== arena

function openMatch(matchId, seed) {
  State.match = matchId;
  State.activeTask = null;
  closeAllStreams();
  $('#tab-arena').disabled = false;
  showView('arena');
  if (seed) applySnapshot(seed);
  const source = new EventSource(`/api/matches/${encodeURIComponent(matchId)}/stream`);
  State.scoreStream = source;
  setConn('live', 'live');
  source.addEventListener('snapshot', (event) => applySnapshot(JSON.parse(event.data)));
  source.addEventListener('end', () => { setConn('', 'finished'); source.close(); refreshHistory(); });
  source.onerror = () => setConn('error', 'reconnecting…');
}

async function cancelMatch() {
  if (!State.match) return;
  if (!confirm('Stop this match? Running trials are terminated and their containers stopped.')) return;
  try { await api(`/api/matches/${State.match}/cancel`, { method: 'POST' }); } catch (e) { alert(e); }
}

function applySnapshot(snapshot) {
  State.snapshot = snapshot;
  $('#arena-name').textContent = snapshot.match.name;
  $('#arena-sub').textContent =
    `${snapshot.dataset.title} · ${snapshot.rows.length} tasks · ${snapshot.match.contestants.length} contestants`
    + (snapshot.match.record_video ? ` · recording ${snapshot.recording_active} live` : '');
  $('#arena-elapsed').textContent = fmt.clock(snapshot.elapsed);
  const pill = $('#arena-status');
  pill.textContent = snapshot.status;
  pill.className = 'pill is-' + snapshot.status;
  const note = $('#arena-note');
  note.hidden = !snapshot.note;
  note.textContent = snapshot.note || '';

  renderScoreboard(snapshot);
  renderRace(snapshot);
  renderRail(snapshot);
  if (State.activeTask) renderLaneHeads(snapshot);
}

// -- scoreboard ------------------------------------------------------------

function renderScoreboard(snapshot) {
  const leaders = snapshot.leaders || {};
  const crowns = {};
  for (const winners of Object.values(leaders)) for (const id of winners) crowns[id] = (crowns[id] || 0) + 1;
  const top = Math.max(0, ...Object.values(crowns));

  $('#scoreboard').replaceChildren(...snapshot.contestants.map((c) => {
    const t = c.totals;
    const isLeader = top > 0 && crowns[c.id] === top;
    const best = (key) => (leaders[key] || []).includes(c.id);

    const stat = (key, label, value) =>
      el('div', { class: 'stat' + (best(key) ? ' is-best' : '') },
        el('span', { class: 'stat-k' }, label),
        el('span', { class: 'stat-v' }, value));

    return el('div', { class: 'card' + (isLeader ? ' is-leader' : ''), style: `--seat:${c.color}` },
      isLeader ? el('div', { class: 'card-crown' }, '👑') : el('div', { class: 'card-rank' }, `${crowns[c.id] || 0}/6`),
      el('div', { class: 'card-name' }, el('span', { class: 'card-swatch' }), c.name),
      el('div', { class: 'card-engine' }, `${c.agent} · ${c.engine_label}`),
      el('div', { class: 'card-state' },
        `${c.state}${t.running ? ` · ${t.running} running` : ''} · ${t.judged}/${t.trials} judged`),
      // An arm whose trials never started has not lost them. Saying so beside
      // the rate is the difference between "won 8 of 10" and "won 8 of the 8
      // that managed to start" — one of which survives being checked.
      t.infrastructure ? el('div', { class: 'card-infra' },
        `${t.infrastructure} never started (harness/host)`) : null,
      el('div', { class: 'card-solve' },
        el('span', { class: 'solve-big' }, fmt.pct(t.solve_rate)),
        el('span', { class: 'solve-frac' }, `${t.passed} / ${t.judged || 0} solved`)),
      el('div', { class: 'card-bar' },
        el('i', { style: `width:${Math.min(100, t.solve_rate)}%` })),
      el('div', { class: 'card-stats' },
        stat('clock_time', 'clock', fmt.clock(t.clock_time)),
        stat('total_cost', 'cost', fmt.money(t.total_cost)),
        stat('tokens_in', 'tok in', fmt.tokens(t.tokens_in)),
        stat('tokens_out', 'tok out', fmt.tokens(t.tokens_out)),
        stat('cache_read', 'cache r', fmt.tokens(t.cache_read)),
        stat('cache_write', 'cache w', fmt.tokens(t.cache_write)),
      ),
      (c.warnings || []).length ? el('div', { class: 'card-warn' }, c.warnings.join(' · ')) : null,
      (c.notes || []).length ? el('div', { class: 'card-note' }, c.notes.join(' · ')) : null,
      c.error ? el('div', { class: 'card-warn' }, c.error) : null,
    );
  }));
}

/* The race: one row per dimension, one bar per contestant, normalised so the
   leader's bar is full. Bars are relative because the absolute numbers span
   orders of magnitude across dimensions — the question this answers is "who
   is ahead and by how much", not "how many tokens exactly". */
function renderRace(snapshot) {
  const leaders = snapshot.leaders || {};
  const rows = snapshot.dimensions.map((dim) => {
    const values = snapshot.contestants.map((c) => Number(c.totals[dim.key]) || 0);
    const max = Math.max(...values, 0);
    const positives = values.filter((v) => v > 0);
    const min = positives.length ? Math.min(...positives) : 0;

    const bars = snapshot.contestants.map((c, index) => {
      const value = values[index];
      let width = 0;
      if (dim.direction === 'lower') {
        // Invert: the smallest non-zero value gets the longest bar. A zero
        // means "has not spent anything yet", which is not a lead — it draws
        // empty rather than winning the row by default.
        width = value > 0 && min > 0 ? (min / value) * 100 : 0;
      } else {
        width = max > 0 ? (value / max) * 100 : 0;
      }
      const isBest = (leaders[dim.key] || []).includes(c.id);
      return el('div', { class: 'race-bar' + (isBest ? ' is-best' : ''), style: `--seat:${c.color}` },
        el('div', { class: 'race-fill' }, el('i', { style: `width:${Math.max(2, Math.min(100, width))}%` })),
        el('div', { class: 'race-value' }, fmt.dim(dim.key, value)),
      );
    });

    const arrow = dim.direction === 'higher' ? '↑' : dim.direction === 'lower' ? '↓' : '·';
    return el('div', { class: 'race-row' },
      el('div', { class: 'race-label', title: dim.blurb }, dim.label, el('i', {}, arrow)),
      el('div', { class: 'race-track' }, ...bars),
    );
  });
  $('#race').replaceChildren(...rows);
}

// -- task rail -------------------------------------------------------------

function renderRail(snapshot) {
  const done = snapshot.rows.filter((row) =>
    Object.values(row.cells).every((cell) => cell && cell.status === 'done')).length;
  $('#rail-progress').textContent = `${done}/${snapshot.rows.length}`;

  $('#rail-list').replaceChildren(...snapshot.rows.map((row) =>
    el('div', {
      class: 'rail-item' + (State.activeTask === row.task ? ' is-active' : ''),
      onclick: () => selectTask(row.task),
    },
      el('div', { class: 'rail-name', title: row.task }, row.task),
      el('div', { class: 'rail-cells' }, ...snapshot.contestants.map((c) => {
        const cell = row.cells[c.id];
        let cls = 'cellbit';
        if (cell) {
          if (cell.status === 'running') cls += ' is-running';
          else if (cell.resolved === true) cls += ' is-pass';
          else if (cell.resolved === false) cls += ' is-fail';
        }
        return el('span', { class: cls, style: `--seat:${c.color}`, title: c.name });
      })),
    )));
}

// -- transcripts -----------------------------------------------------------

function closeAllStreams() {
  for (const source of State.streams.values()) source.close();
  State.streams.clear();
}

function selectTask(task) {
  if (State.activeTask === task) return;
  State.activeTask = task;
  closeAllStreams();
  const snapshot = State.snapshot;
  $('#stage-empty').hidden = true;
  $('#stage-live').hidden = false;
  $('#stage-task').textContent = task;
  renderRail(snapshot);

  const lanes = $('#lanes');
  lanes.style.gridTemplateColumns = `repeat(${Math.min(snapshot.contestants.length, 3)}, minmax(0, 1fr))`;
  lanes.replaceChildren(...snapshot.contestants.map((c) => laneNode(c, task)));
  renderLaneHeads(snapshot);
  for (const c of snapshot.contestants) openTranscript(c, task);
}

function laneNode(contestant, task) {
  return el('div', { class: 'lane', id: `lane-${contestant.id}`, style: `--seat:${contestant.color}` },
    el('div', { class: 'lane-head' },
      el('span', { class: 'lane-dot' }),
      el('span', { class: 'lane-name' }, contestant.name),
      el('span', { class: 'lane-verdict v-wait' }, '…')),
    el('div', { class: 'lane-metrics' }),
    el('div', { class: 'lane-video', hidden: true }),
    el('div', { class: 'feed' + (State.showReasoning ? '' : ' hide-reasoning') }),
  );
}

function renderLaneHeads(snapshot) {
  const row = snapshot.rows.find((r) => r.task === State.activeTask);
  if (!row) return;
  for (const c of snapshot.contestants) {
    const lane = $(`#lane-${c.id}`);
    if (!lane) continue;
    const cell = row.cells[c.id];
    const verdict = $('.lane-verdict', lane);
    if (!cell) { verdict.className = 'lane-verdict v-wait'; verdict.textContent = 'queued'; continue; }
    if (cell.status === 'done') {
      const pass = cell.resolved === true;
      verdict.className = 'lane-verdict ' + (pass ? 'v-pass' : 'v-fail');
      verdict.textContent = pass ? '✓ SOLVED' : (cell.failure || '✗ failed');
    } else {
      verdict.className = 'lane-verdict v-run';
      verdict.textContent = cell.status + (cell.age_s != null ? ` ${Math.round(cell.age_s)}s` : '');
    }
    $('.lane-metrics', lane).replaceChildren(...[
      el('span', {}, 'steps ', el('b', {}, String(cell.steps))),
      el('span', {}, 'tools ', el('b', {}, String(cell.tools))),
      el('span', {}, 'in ', el('b', {}, fmt.tokens(cell.tokens_in))),
      el('span', {}, 'out ', el('b', {}, fmt.tokens(cell.tokens_out))),
      el('span', {}, 'cache ', el('b', {}, `${fmt.tokens(cell.cache_read)}/${fmt.tokens(cell.cache_write)}`)),
      el('span', {}, 'cost ', el('b', {}, fmt.money(cell.total_cost))),
      el('span', {}, fmt.clock(cell.clock_time)),
      // `.filter(Boolean)`: replaceChildren() stringifies a bare null into the
      // literal text "null", which is how a metrics row read `32:10 null`.
      cell.cap_hits ? el('span', { style: 'color:var(--warn)' }, `⚠ ${cell.cap_hits} output-cap`) : null,
    ].filter(Boolean));

    const videoBox = $('.lane-video', lane);
    if (cell.has_video && videoBox.hidden) {
      videoBox.hidden = false;
      videoBox.replaceChildren(el('details', {},
        el('summary', {}, '▸ screen recording (MP4)'),
        el('video', {
          controls: true, preload: 'none',
          src: `/api/matches/${State.match}/video/${encodeURIComponent(c.id)}/${encodeURIComponent(State.activeTask)}`,
        })));
    }
  }
}

function openTranscript(contestant, task) {
  const key = `${contestant.id}:${task}`;
  const url = `/api/matches/${encodeURIComponent(State.match)}/transcript/`
            + `${encodeURIComponent(contestant.id)}/${encodeURIComponent(task)}`;
  const source = new EventSource(url);
  State.streams.set(key, source);

  const lane = $(`#lane-${contestant.id}`);
  const feed = $('.feed', lane);
  const nodes = new Map();   // seq -> element, so a growing entry replaces itself

  source.addEventListener('waiting', () => {
    if (!nodes.size) feed.replaceChildren(el('div', { class: 'ent k-stage' },
      el('span', { class: 'ent-t' }, ''), el('span', { class: 'ent-b' }, 'waiting for the trial to start…')));
  });

  source.addEventListener('entries', (event) => {
    const { entries } = JSON.parse(event.data);
    if (nodes.size === 0) feed.replaceChildren();
    for (const entry of entries) {
      const node = entryNode(entry);
      const existing = nodes.get(entry.seq);
      if (existing) existing.replaceWith(node);
      else feed.append(node);
      nodes.set(entry.seq, node);
    }
    if (State.follow) feed.scrollTop = feed.scrollHeight;
  });

  source.addEventListener('end', () => { source.close(); State.streams.delete(key); });
  source.onerror = () => { /* the browser retries; a closed trial ends above */ };
}

function entryNode(entry) {
  const meta = entry.meta || {};
  let body = entry.body || '';

  if (entry.kind === 'usage') {
    body = `${meta.model || ''} · in ${fmt.tokens(meta.tokens_in)} out ${fmt.tokens(meta.tokens_out)}`
         + ` · cache ${fmt.tokens(meta.cache_read)}/${fmt.tokens(meta.cache_write)}`
         + ` · ${fmt.money(meta.cost_usd)} · ${Math.round((meta.duration_ms || 0) / 100) / 10}s`;
  } else if (entry.kind === 'tool') {
    body = `${entry.title} ${body}`;
  } else if (entry.kind === 'tool_result') {
    const lines = body.split('\n');
    body = lines.slice(0, 12).join('\n') + (lines.length > 12 ? `\n… +${lines.length - 12} lines` : '');
  } else if (entry.kind === 'stage' || entry.kind === 'complete' || entry.kind === 'verdict') {
    body = entry.title + (body ? ` — ${body}` : '');
  }

  return el('div', { class: `ent k-${entry.kind}` },
    el('span', { class: 'ent-t' }, fmt.clock(entry.t)),
    el('span', { class: 'ent-b' }, body),
  );
}

// ---------------------------------------------------------------------- go

document.addEventListener('DOMContentLoaded', boot);
