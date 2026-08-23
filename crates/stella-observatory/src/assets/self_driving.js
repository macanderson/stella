/* The self-driving agents view: what the loop is doing this second, what it
   has delivered, what it is about to take, and what it learned on the way.

   Served at /assets/self_driving.js and loaded after index.html's inline
   script, so the page's helpers (esc, num, fmtInt, fmtUsd, agoUnix, api, $)
   are in scope. Everything it draws comes from /api/self-driving-sessions and
   /api/self-driving-session — the journal the loop writes, the queue
   snapshot it leaves, context.db and store.db — nothing is fetched from
   anywhere else (the CSP forbids it).

   Charts are inline SVG with CSS-driven motion: bars grow from the baseline,
   lines draw themselves, counters tween to their new value, and a running
   session carries a pulse. Motion is only ever applied to a value that
   changed, so a page left open does not twitch. `prefers-reduced-motion`
   turns all of it off. */
(() => {
"use strict";

const S = { data: null, session: null, detail: null, drawn: {}, tween: {} };
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

/* ── styles ───────────────────────────────────────────────────────────────
   Scoped under #panel-self-driving so nothing here can restyle another tab. */
const css = `
#panel-self-driving .sd-hero{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:var(--sp2);margin-bottom:var(--sp2)}
#panel-self-driving .sd-hero .tile{position:relative;overflow:hidden}
#panel-self-driving .sd-hero .tile .v{transition:color var(--dur-state)}
#panel-self-driving .sd-pulse{display:inline-block;width:9px;height:9px;border-radius:50%;background:var(--ok);position:relative;vertical-align:middle;margin-right:6px}
#panel-self-driving .sd-pulse::after{content:"";position:absolute;inset:-4px;border-radius:50%;border:1px solid var(--ok);opacity:0;animation:sd-ring 1.8s ease-out infinite}
#panel-self-driving .sd-pulse.idle{background:var(--text-3)}
#panel-self-driving .sd-pulse.idle::after{animation:none}
@keyframes sd-ring{0%{transform:scale(.5);opacity:.8}100%{transform:scale(1.9);opacity:0}}
#panel-self-driving .sd-agent{border:1px solid var(--hairline-strong);padding:var(--sp2);margin-bottom:var(--sp1);background:var(--raised);display:grid;grid-template-columns:auto 1fr auto;gap:var(--sp2);align-items:start;animation:sd-in var(--dur-reveal) var(--ease-reveal)}
#panel-self-driving .sd-agent .who{font:500 var(--fs-base)/1.4 var(--mono)}
#panel-self-driving .sd-agent .what{font:var(--fw-prose) var(--fs-base)/1.5 inherit;color:var(--text-2);margin-top:2px}
#panel-self-driving .sd-agent .meta{font:var(--fs-micro)/1.5 var(--mono);color:var(--text-3);text-align:right;white-space:nowrap}
#panel-self-driving .sd-agent .stage{display:inline-block;margin-top:6px}
@keyframes sd-in{from{opacity:0;transform:translateY(4px)}to{opacity:1;transform:none}}
#panel-self-driving .sd-stack{display:flex;height:14px;border:1px solid var(--hairline-strong);background:var(--sunken);overflow:hidden;margin:6px 0 10px}
#panel-self-driving .sd-stack > div{height:100%;transition:width .6s var(--ease-reveal);min-width:0}
#panel-self-driving .sd-stack .P0{background:var(--bad)}
#panel-self-driving .sd-stack .P1{background:var(--warn)}
#panel-self-driving .sd-stack .P2{background:var(--c2)}
#panel-self-driving .sd-stack .P3{background:var(--c3)}
#panel-self-driving .sd-stack .untriaged{background:var(--c4)}
#panel-self-driving .sd-legend{display:flex;flex-wrap:wrap;gap:var(--sp2);font:var(--fs-micro)/1.4 var(--mono);color:var(--text-3);margin-bottom:var(--sp1)}
#panel-self-driving .sd-legend i{display:inline-block;width:9px;height:9px;margin-right:5px;vertical-align:-1px}
#panel-self-driving .sd-qgroup{margin-top:var(--sp1)}
#panel-self-driving .sd-qgroup .kick{margin-bottom:3px}
#panel-self-driving .sd-q{display:grid;grid-template-columns:auto 1fr auto;gap:var(--sp1);padding:3px 0;border-bottom:1px solid var(--hairline);font:var(--fw-prose) var(--fs-sm)/1.45 inherit}
#panel-self-driving .sd-q .n{font:var(--fs-sm)/1.45 var(--mono);color:var(--text-3)}
#panel-self-driving .sd-q .t{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#panel-self-driving svg.sd-chart{width:100%;height:auto;display:block}
#panel-self-driving svg.sd-chart .bar{transform-origin:bottom;transform-box:fill-box;transition:transform .7s var(--ease-reveal)}
#panel-self-driving svg.sd-chart .bar.enter{transform:scaleY(0)}
#panel-self-driving svg.sd-chart .line{fill:none;stroke-width:1.6;stroke-linejoin:round;stroke-linecap:round;transition:stroke-dashoffset 1.1s var(--ease-reveal)}
#panel-self-driving svg.sd-chart .dot{transition:r .4s var(--ease-reveal)}
#panel-self-driving svg.sd-chart text{font:var(--fs-micro) var(--mono);fill:var(--text-3)}
#panel-self-driving svg.sd-chart .axis{stroke:var(--hairline-strong);stroke-width:1}
#panel-self-driving .sd-funnel{display:grid;gap:6px;margin-bottom:var(--sp2)}
#panel-self-driving .sd-funnel .row{display:grid;grid-template-columns:92px 1fr 64px 56px;gap:var(--sp1);align-items:center;font:var(--fs-sm)/1.4 var(--mono)}
#panel-self-driving .sd-funnel .row .k{color:var(--text-2)}
#panel-self-driving .sd-funnel .bar{height:12px;background:var(--sunken);border:1px solid var(--hairline-strong);overflow:hidden}
#panel-self-driving .sd-funnel .bar > div{height:100%;background:var(--mark);transition:width .7s var(--ease-reveal);width:0}
#panel-self-driving .sd-funnel .row .v{text-align:right}
#panel-self-driving .sd-funnel .row .c{text-align:right;color:var(--text-3)}
#panel-self-driving svg.sd-gantt{width:100%;height:auto;display:block}
#panel-self-driving svg.sd-gantt rect.s{transition:width .6s var(--ease-reveal),x .6s var(--ease-reveal)}
#panel-self-driving svg.sd-gantt rect.s.running{animation:sd-breathe 2.2s ease-in-out infinite}
#panel-self-driving svg.sd-gantt text{font:var(--fs-micro) var(--mono);fill:var(--text-3)}
#panel-self-driving svg.sd-gantt .conc{fill:none;stroke:var(--ok);stroke-width:1.2;opacity:.9}
@keyframes sd-breathe{0%,100%{opacity:1}50%{opacity:.55}}
#panel-self-driving #sd-sessions tr{cursor:pointer}
#panel-self-driving #sd-sessions tr.sel td{background:var(--accent-wash)}
#panel-self-driving .sd-issue{border:1px solid var(--hairline);padding:var(--sp1) var(--sp2);margin-top:var(--sp1);background:var(--raised)}
#panel-self-driving .sd-issue summary{cursor:pointer;list-style:none;display:grid;grid-template-columns:auto 1fr auto;gap:var(--sp1);align-items:center;font:var(--fs-sm)/1.45 inherit}
#panel-self-driving .sd-issue summary::-webkit-details-marker{display:none}
#panel-self-driving .sd-issue .n{font:var(--fs-sm) var(--mono);color:var(--text-3)}
#panel-self-driving .sd-steps{margin:var(--sp1) 0 0 6px;padding-left:var(--sp2);border-left:1px solid var(--hairline-strong);display:grid;gap:6px}
#panel-self-driving .sd-step{position:relative;font:var(--fw-prose) var(--fs-sm)/1.45 inherit;color:var(--text-2);animation:sd-in var(--dur-reveal) var(--ease-reveal) both}
#panel-self-driving .sd-step::before{content:"";position:absolute;left:calc(-1 * var(--sp2) - 4px);top:7px;width:7px;height:7px;border-radius:50%;background:var(--hairline-strong)}
#panel-self-driving .sd-step.ok::before{background:var(--ok)}
#panel-self-driving .sd-step.bad::before{background:var(--bad)}
#panel-self-driving .sd-step.warn::before{background:var(--warn)}
#panel-self-driving .sd-step .when{font:var(--fs-micro) var(--mono);color:var(--text-3);margin-right:6px}
#panel-self-driving .sd-lesson{display:grid;grid-template-columns:1fr auto;gap:var(--sp2);padding:var(--sp1) 0;border-bottom:1px solid var(--hairline);font:var(--fw-prose) var(--fs-base)/1.55 inherit}
#panel-self-driving .sd-lesson .meta{font:var(--fs-micro)/1.5 var(--mono);color:var(--text-3);text-align:right;white-space:nowrap}
#panel-self-driving .sd-note{font:var(--fw-prose) var(--fs-sm)/1.5 inherit;color:var(--text-3);margin-top:var(--sp1)}
#panel-self-driving .sd-updated{font:var(--fs-micro)/1.4 var(--mono);color:var(--text-3);text-align:right;margin:-6px 0 var(--sp1)}
@media (prefers-reduced-motion: reduce){
  #panel-self-driving *{animation:none!important;transition:none!important}
  #panel-self-driving svg.sd-chart .bar.enter{transform:none}
}
`;
const style = document.createElement("style");
style.textContent = css;
document.head.appendChild(style);

/* ── helpers ─────────────────────────────────────────────────────────────── */
const el = (id) => document.getElementById(id);
const dur = (s) => {
  s = num(s);
  if (s < 60) return s + "s";
  if (s < 3600) return Math.floor(s / 60) + "m " + (s % 60) + "s";
  if (s < 86400) return Math.floor(s / 3600) + "h " + Math.floor((s % 3600) / 60) + "m";
  return Math.floor(s / 86400) + "d " + Math.floor((s % 86400) / 3600) + "h";
};
const clock = (u) => {
  if (!u) return "—";
  const d = new Date(u * 1000);
  return d.toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
};
const dayLabel = (day) => day.slice(5);
const STATUS = {
  running: ["ok", "running"], stopped: ["dim", "stopped"],
  crashed: ["bad", "crashed"], lost: ["warn", "lost"],
};
const badge = (status) => {
  const [cls, label] = STATUS[status] ?? ["dim", status || "—"];
  return `<span class="badge ${cls}">${esc(label)}</span>`;
};
const OUTCOME = {
  merged: "ok", "pr open": "accent", changed: "ok", working: "accent", retrying: "warn",
  "no change": "dim", failed: "bad", deferred: "dim", escalated: "warn", unknown: "dim",
};
const outcomeBadge = (o) => `<span class="badge ${OUTCOME[o] ?? "dim"}">${esc(o)}</span>`;
const RANKS = ["P0", "P1", "P2", "P3", "untriaged"];

/* Tween a counter from what it last showed to its new value. A number that
   did not change is written once and left alone. */
function count(id, value, fmt) {
  const node = el(id);
  if (!node) return;
  const to = num(value);
  const from = S.tween[id] ?? to;
  S.tween[id] = to;
  if (reduced || from === to) { node.textContent = fmt(to); return; }
  const t0 = performance.now(), ms = 650;
  const step = (t) => {
    const k = Math.min(1, (t - t0) / ms), e = 1 - Math.pow(1 - k, 3);
    node.textContent = fmt(from + (to - from) * e);
    if (k < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

/* ── hero ────────────────────────────────────────────────────────────────── */
function renderHero(d) {
  const t = d.totals ?? {};
  const live = num(t.running) > 0;
  const yieldPct = num(t.claimed) ? Math.round(100 * num(t.merged) / num(t.claimed)) : 0;
  el("sd-hero").innerHTML = [
    ["Agents running", "sd-k-running", `<span class="sd-pulse${live ? "" : " idle"}"></span><span id="sd-k-running"></span>`,
      `${fmtInt(t.busy)} busy · ${fmtInt(t.holders)} holding a lease`, live],
    ["Issues merged", "sd-k-merged", `<span id="sd-k-merged"></span>`, `${fmtInt(t.claimed)} claimed · ${yieldPct}% yield`],
    ["Sessions", "sd-k-sessions", `<span id="sd-k-sessions"></span>`, `${fmtInt(t.loops)} repositor${num(t.loops) === 1 ? "y" : "ies"} on this machine`],
    ["Lessons learned", "sd-k-lessons", `<span id="sd-k-lessons"></span>`, `${fmtInt(t.applied)} applied in later prompts`],
    ["Spend", "sd-k-spend", `<span id="sd-k-spend"></span>`,
      t.usd_per_merge != null ? `${fmtUsd(t.usd_per_merge)} per merged fix` : "no merges to price yet"],
  ].map(([k, , v, sub, accent]) =>
    `<div class="card tile"><div class="k">${k}</div><div class="v${accent ? " accent" : ""}">${v}</div><div class="sub">${sub}</div></div>`
  ).join("");
  count("sd-k-running", t.running, (v) => fmtInt(Math.round(v)));
  count("sd-k-merged", t.merged, (v) => fmtInt(Math.round(v)));
  count("sd-k-sessions", t.sessions, (v) => fmtInt(Math.round(v)));
  count("sd-k-lessons", t.lessons, (v) => fmtInt(Math.round(v)));
  count("sd-k-spend", t.spend_usd, (v) => fmtUsd(v));
}

/* ── right now ───────────────────────────────────────────────────────────── */
function renderLive(d) {
  const running = (d.sessions ?? []).filter((s) => s.status === "running");
  const claims = (d.loops ?? []).flatMap((l) => (l.claims ?? []).map((c) => ({ ...c, slug: l.slug })));
  el("sd-live-count").innerHTML = running.length
    ? `<span class="badge ok">${running.length} live</span>` : `<span class="badge dim">idle</span>`;
  if (!running.length) {
    el("sd-live").innerHTML = `<div class="empty">No self-driving agent is running on this machine.<br>
      Start one with <code>stella self-driving drive</code> in a repository.</div>`;
    return;
  }
  el("sd-live").innerHTML = running.map((s) => {
    const open = (s.issues ?? []).filter((i) => i.open);
    const cur = open[0];
    const what = cur
      ? `<b>#${esc(cur.number)}</b> ${esc(cur.title || "")}`
      : `between issues — asking the queue`;
    const stage = cur
      ? `${outcomeBadge(cur.outcome)}${cur.pr ? ` <span class="badge dim">PR #${esc(cur.pr)}</span>` : ""}${
          cur.polls ? ` <span class="sd-note" style="display:inline">${fmtInt(cur.polls)} polls waiting</span>` : ""}`
      : "";
    const lease = claims.find((c) => c.pid && c.pid === s.pid);
    return `<div class="sd-agent" data-session="${esc(s.id)}">
      <span class="sd-pulse" title="${esc(s.liveness)}"></span>
      <div><div class="who">${esc(s.slug)} · ${esc(s.id)}</div>
        <div class="what">${what}</div>
        <div class="stage">${stage}${lease ? ` <span class="badge ok">lease ${dur(lease.held_secs)}</span>` : ""}</div></div>
      <div class="meta">up ${dur(s.seconds)}<br>${fmtUsd(s.spend_usd)} spent<br>${fmtInt(s.prs_merged)} merged · ${fmtInt(s.claimed)} claimed</div>
    </div>`;
  }).join("") + (running.length === 1
    ? `<div class="sd-note">One agent works one issue at a time. Parallelism is one more
       <code>stella self-driving drive</code> per repository — they share the lease table, so two never take the same issue.</div>`
    : "");
}

/* ── queue ───────────────────────────────────────────────────────────────── */
function renderQueue(d) {
  const loops = (d.loops ?? []).filter((l) => (l.queue?.items ?? []).length);
  const byRank = d.queue_by_rank ?? {};
  const total = RANKS.reduce((a, r) => a + num(byRank[r]), 0);
  if (!total) {
    el("sd-queue").innerHTML = `<div class="empty">No queue snapshot yet. The loop writes one each time it ranks
      the backlog — it appears after the next claim pass of a <code>drive</code> built with this change.</div>`;
    return;
  }
  const stack = `<div class="sd-stack">${RANKS.filter((r) => num(byRank[r]))
    .map((r) => `<div class="${r}" style="width:${(100 * num(byRank[r]) / total).toFixed(1)}%" title="${r}: ${fmtInt(byRank[r])}"></div>`).join("")}</div>`;
  const legend = `<div class="sd-legend">${RANKS.filter((r) => num(byRank[r]))
    .map((r) => `<span><i class="${r}" style="background:var(--${{ P0: "bad", P1: "warn", P2: "c2", P3: "c3", untriaged: "c4" }[r]})"></i>${r} ${fmtInt(byRank[r])}</span>`).join("")}
    <span style="margin-left:auto">${fmtInt(total)} open defects</span></div>`;
  const groups = loops.map((l) => {
    const q = l.queue;
    const items = q.items ?? [];
    const at = q.at ? `as of ${esc(q.at)}` : q.source === "legacy-cache" ? "from the loop's cached listing (undated)" : "";
    const rows = RANKS.map((r) => {
      const of = items.filter((i) => (i.rank || "untriaged") === r);
      if (!of.length) return "";
      return `<div class="sd-qgroup"><div class="kick">${r} · ${of.length}</div>${of.slice(0, 6).map((i) =>
        `<div class="sd-q"><span class="n">#${esc(i.number)}</span><span class="t" title="${esc(i.title)}">${
          i.url ? `<a href="${esc(i.url)}" target="_blank" rel="noopener">${esc(i.title)}</a>` : esc(i.title)}</span>
          <span>${i.in_progress ? `<span class="badge ok">in progress</span>` : ""}</span></div>`).join("")}${
        of.length > 6 ? `<div class="sd-note">+${of.length - 6} more</div>` : ""}</div>`;
    }).join("");
    return `<div class="kick">${esc(l.slug)}${l.is_current_workspace ? " · here" : ""} · ${at}</div>${rows}`;
  }).join("<hr style='border:0;border-top:1px solid var(--hairline);margin:var(--sp2) 0'>");
  el("sd-queue").innerHTML = stack + legend + `<div class="scroll-y">${groups}</div>`;
}

/* ── delivery & learning chart ───────────────────────────────────────────── */
function renderChart(d) {
  const days = (d.daily ?? []).slice(-45);
  const host = el("sd-chart");
  if (!days.length) { host.innerHTML = `<div class="empty">Nothing delivered yet.</div>`; return; }
  const W = 640, H = 220, L = 34, R = 34, T = 14, B = 30;
  const iw = W - L - R, ih = H - T - B;
  const maxBar = Math.max(1, ...days.map((xd) => num(xd.merged) + num(xd.changed)));
  let cum = 0;
  const cumL = days.map((xd) => (cum += num(xd.lessons)));
  const maxL = Math.max(1, cum, ...days.map((xd) => num(xd.applied)));
  const bw = iw / days.length;
  const x = (i) => L + i * bw;
  const yB = (v) => T + ih - ih * v / maxBar;
  const yL = (v) => T + ih - ih * v / maxL;
  const key = days.map((xd) => `${xd.day}:${xd.merged}:${xd.changed}:${xd.lessons}:${xd.applied}`).join("|");
  const fresh = S.drawn.chart !== key;
  S.drawn.chart = key;

  const bars = days.map((xd, i) => {
    const m = num(xd.merged), c = num(xd.changed);
    const w = Math.max(2, bw * 0.62);
    const bx = x(i) + (bw - w) / 2;
    return `<rect class="bar${fresh ? " enter" : ""}" x="${bx.toFixed(1)}" y="${yB(m + c).toFixed(1)}" width="${w.toFixed(1)}" height="${(ih - (yB(m + c) - T)).toFixed(1)}" fill="var(--c3)"><title>${xd.day}: ${c} changed</title></rect>
      <rect class="bar${fresh ? " enter" : ""}" x="${bx.toFixed(1)}" y="${yB(m).toFixed(1)}" width="${w.toFixed(1)}" height="${(ih - (yB(m) - T)).toFixed(1)}" fill="var(--mark)"><title>${xd.day}: ${m} merged</title></rect>`;
  }).join("");
  const pts = days.map((_, i) => `${(x(i) + bw / 2).toFixed(1)},${yL(cumL[i]).toFixed(1)}`);
  const path = "M" + pts.join(" L");
  const dots = days.map((xd, i) => num(xd.applied)
    ? `<circle class="dot" cx="${(x(i) + bw / 2).toFixed(1)}" cy="${yL(num(xd.applied)).toFixed(1)}" r="${fresh ? 0 : 3.5}" fill="var(--warn)"><title>${xd.day}: ${xd.applied} lesson(s) applied in prompts</title></circle>` : "").join("");
  const spend = days.map((xd, i) => num(xd.spend_usd)
    ? `<text x="${(x(i) + bw / 2).toFixed(1)}" y="${H - 4}" text-anchor="middle" fill="var(--text-3)">${fmtUsd(xd.spend_usd)}</text>` : "").join("");
  const labels = days.map((xd, i) => (days.length <= 12 || i % Math.ceil(days.length / 10) === 0)
    ? `<text x="${(x(i) + bw / 2).toFixed(1)}" y="${H - 16}" text-anchor="middle">${dayLabel(xd.day)}</text>` : "").join("");

  host.innerHTML = `<div class="sd-legend">
      <span><i style="background:var(--mark)"></i>merged</span><span><i style="background:var(--c3)"></i>changed, not yet merged</span>
      <span><i style="background:var(--ok)"></i>lessons learned (cumulative)</span><span><i style="background:var(--warn)"></i>lessons applied</span></div>
    <svg class="sd-chart" viewBox="0 0 ${W} ${H}" role="img" aria-label="Issues merged and changed per day against lessons learned and applied">
      <line class="axis" x1="${L}" y1="${T + ih}" x2="${W - R}" y2="${T + ih}"/>
      <text x="${L - 6}" y="${T + 8}" text-anchor="end">${maxBar}</text>
      <text x="${W - R + 6}" y="${T + 8}">${maxL}</text>
      ${bars}
      <path class="line" d="${path}" stroke="var(--ok)" pathLength="1" style="stroke-dasharray:1;stroke-dashoffset:${fresh ? 1 : 0}"/>
      ${dots}${labels}${spend}
    </svg>
    <div class="sd-note">Left axis: issues per day. Right axis: lessons. Dollar marks are the day's metered spend.
      A lesson is attributed to an issue when it was recorded during that issue's turn — a time-window join, stated as such.</div>`;
  if (fresh && !reduced) requestAnimationFrame(() => requestAnimationFrame(() => {
    host.querySelectorAll(".bar.enter").forEach((b) => b.classList.remove("enter"));
    host.querySelectorAll(".line").forEach((p) => { p.style.strokeDashoffset = 0; });
    host.querySelectorAll(".dot").forEach((c) => c.setAttribute("r", 3.5));
  }));
}

/* ── funnel ──────────────────────────────────────────────────────────────── */
function renderFunnel(d) {
  const t = d.totals ?? {};
  const s = d.sessions ?? [];
  const sum = (k) => s.reduce((a, x) => a + num(x[k]), 0);
  const rows = [
    ["claimed", num(t.claimed)], ["changed", sum("changed")],
    ["PR opened", sum("prs_opened")], ["merged", num(t.merged)],
  ];
  const top = Math.max(1, rows[0][1]);
  const failed = sum("failed"), nochange = sum("no_change"), deferred = sum("deferred"), escalated = sum("escalated");
  const key = rows.map((r) => r[1]).join(":");
  const fresh = S.drawn.funnel !== key; S.drawn.funnel = key;
  el("sd-funnel").innerHTML = `<div class="sd-funnel">${rows.map(([k, v], i) => {
    const prev = i ? rows[i - 1][1] : v;
    return `<div class="row"><span class="k">${k}</span><div class="bar"><div data-w="${(100 * v / top).toFixed(1)}"${fresh ? "" : ` style="width:${(100 * v / top).toFixed(1)}%"`}></div></div>
      <span class="v">${fmtInt(v)}</span><span class="c">${prev ? Math.round(100 * v / prev) + "%" : "—"}</span></div>`;
  }).join("")}</div>
  <div class="sd-note">Left behind: ${fmtInt(nochange)} no change · ${fmtInt(failed)} failed · ${fmtInt(deferred)} deferred · ${fmtInt(escalated)} escalated.
    Spend ${fmtUsd(t.spend_usd)} across ${fmtInt(t.sessions)} sessions${t.usd_per_merge != null ? ` — ${fmtUsd(t.usd_per_merge)} per merged fix` : ""}.</div>`;
  if (fresh) requestAnimationFrame(() => requestAnimationFrame(() => {
    el("sd-funnel").querySelectorAll(".bar > div").forEach((b) => { b.style.width = b.dataset.w + "%"; });
  }));
}

/* ── session timeline (concurrency) ──────────────────────────────────────── */
function renderGantt(d) {
  const all = (d.sessions ?? []).filter((s) => s.started_unix > 0).slice(0, 40).reverse();
  const host = el("sd-gantt");
  if (!all.length) { host.innerHTML = `<div class="empty">No sessions yet.</div>`; return; }
  const now = num(d.generated_unix) || Math.floor(Date.now() / 1000);
  const start = Math.min(...all.map((s) => s.started_unix));
  const end = Math.max(now, ...all.map((s) => s.started_unix + num(s.seconds)));
  const span = Math.max(1, end - start);
  const W = 900, L = 200, R = 16, rowH = 14, T = 18;
  const H = T + all.length * rowH + 46;
  const iw = W - L - R;
  const x = (u) => L + iw * (u - start) / span;
  // Concurrency: how many sessions were alive at each boundary.
  const edges = [];
  all.forEach((s) => { edges.push([s.started_unix, 1]); edges.push([s.started_unix + num(s.seconds), -1]); });
  edges.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  let c = 0, maxC = 0; const cpts = [];
  edges.forEach(([u, dlt]) => { c += dlt; maxC = Math.max(maxC, c); cpts.push([u, c]); });
  const cy = (v) => T + all.length * rowH + 24 - 16 * v / Math.max(1, maxC);
  const short = (slug) => (slug.length > 18 ? slug.slice(0, 17) + "…" : slug);
  let cpath = "", last = null;
  cpts.forEach(([u, v]) => { if (last != null) cpath += ` L${x(u).toFixed(1)},${cy(last).toFixed(1)}`; cpath += `${last == null ? "M" : " L"}${x(u).toFixed(1)},${cy(v).toFixed(1)}`; last = v; });
  const rows = all.map((s, i) => {
    const y = T + i * rowH;
    const w = Math.max(2, x(s.started_unix + num(s.seconds)) - x(s.started_unix));
    const fill = { running: "var(--ok)", stopped: "var(--c2)", crashed: "var(--bad)", lost: "var(--warn)" }[s.status] ?? "var(--c3)";
    return `<text x="${L - 6}" y="${y + 10}" text-anchor="end">${esc(short(s.slug))} · ${esc(s.started_at.slice(5, 16).replace("T", " "))}</text>
      <rect class="s ${s.status}" x="${x(s.started_unix).toFixed(1)}" y="${y + 2}" width="${w.toFixed(1)}" height="${rowH - 4}" fill="${fill}" data-session="${esc(s.id)}" style="cursor:pointer">
        <title>${esc(s.id)} · ${s.status} · ${dur(s.seconds)} · ${fmtInt(s.claimed)} claimed, ${fmtInt(s.prs_merged)} merged · ${fmtUsd(s.spend_usd)}</title></rect>`;
  }).join("");
  host.innerHTML = `<svg class="sd-gantt" viewBox="0 0 ${W} ${H}" role="img" aria-label="Every session as a bar on a shared time axis, with concurrency beneath">
    ${rows}
    <path class="conc" d="${cpath}"/>
    <text x="${L - 6}" y="${cy(0) + 4}" text-anchor="end">agents at once · max ${maxC}</text>
    <line class="axis" x1="${L}" y1="${cy(0) + 8}" x2="${W - R}" y2="${cy(0) + 8}" stroke="var(--hairline-strong)"/>
    <text x="${L}" y="${H - 3}">${esc(new Date(start * 1000).toISOString().slice(0, 16).replace("T", " "))}</text>
    <text x="${W - R}" y="${H - 3}" text-anchor="end">now</text>
  </svg>
  <div class="sd-legend"><span><i style="background:var(--ok)"></i>running</span><span><i style="background:var(--c2)"></i>stopped</span>
    <span><i style="background:var(--warn)"></i>lost (journal went quiet, no stop record)</span><span><i style="background:var(--bad)"></i>crashed (pid gone)</span></div>`;
}

/* ── sessions table ──────────────────────────────────────────────────────── */
function renderSessions(d) {
  const s = d.sessions ?? [];
  el("sd-sessions").innerHTML = s.length ? `<table><thead><tr>
      <th>Session</th><th>Status</th><th>Started</th><th class="num">Dur</th><th class="num">Claimed</th>
      <th class="num">Changed</th><th class="num">PRs</th><th class="num">Merged</th><th class="num">Lessons</th><th class="num">Spend</th></tr></thead><tbody>
    ${s.map((r) => `<tr data-session="${esc(r.id)}" class="${S.session === r.id ? "sel" : ""}" title="${esc(r.liveness)}">
      <td>${esc(r.slug)}<br><span class="kick">${esc(r.id)}</span></td>
      <td>${badge(r.status)}</td>
      <td>${agoUnix(r.started_unix)}</td>
      <td class="num">${dur(r.seconds)}</td>
      <td class="num">${fmtInt(r.claimed)}</td>
      <td class="num">${fmtInt(r.changed)}</td>
      <td class="num">${fmtInt(r.prs_opened)}</td>
      <td class="num">${fmtInt(r.prs_merged)}</td>
      <td class="num">${fmtInt(r.lessons)}</td>
      <td class="num">${fmtUsd(r.spend_usd)}</td></tr>`).join("")}</tbody></table>`
    : `<div class="empty">No sessions recorded on this machine.</div>`;
}

const STEP = {
  claimed: ["", "claimed"], work_started: ["", "work started"], work_changed: ["ok", "left changes"],
  work_no_change: ["warn", "no change"], work_failed: ["bad", "failed"], verify_started: ["", "verifying"],
  verified: ["ok", "verified locally"], verify_failed: ["warn", "local check failed"], waived: ["warn", "verification waived"],
  pr_opened: ["ok", "PR opened"], pr_merged: ["ok", "merged"], pr_escalated: ["warn", "PR escalated"],
  deferred: ["", "deferred"], escalated: ["warn", "escalated"], transient: ["warn", "retrying"],
  triage_started: ["", "triaging"], triaged: ["ok", "triaged"], resumed: ["", "resumed"],
};

async function openSession(id, quiet) {
  S.session = id;
  document.querySelectorAll("#sd-sessions tr[data-session]").forEach((tr) => tr.classList.toggle("sel", tr.dataset.session === id));
  let r;
  try { r = await api("/api/self-driving-session?id=" + encodeURIComponent(id)); }
  catch (e) { if (!quiet) fail("Could not load that session.", e); return; }
  if (!r || !r.id) { el("sd-detail").innerHTML = `<div class="empty">That session is no longer on disk.</div>`; return; }
  S.detail = r;
  el("sd-detail-kick").textContent = `${r.slug} · ${r.status} · ${r.liveness}`;
  el("sd-detail-title").innerHTML = `${esc(r.id)} ${badge(r.status)}`;
  const params = (r.params ?? []).map((p) => `<div class="feed-item">${esc(p)}</div>`).join("");
  const issues = (r.issues ?? []).map((i) => {
    const steps = (i.events ?? []).map((e, k, arr) => {
      const [cls, label] = STEP[e.action] ?? ["", e.action];
      const gap = k ? num(e.at_unix) - num(arr[k - 1].at_unix) : 0;
      return `<div class="sd-step ${cls}" style="animation-delay:${Math.min(k, 12) * 40}ms"><span class="when">${clock(e.at_unix)}${gap ? ` +${dur(gap)}` : ""}</span>
        <b>${esc(label)}</b>${e.pr ? ` <span class="badge dim">PR #${esc(e.pr)}</span>` : ""} — ${esc(e.outcome)}</div>`;
    }).join("");
    return `<details class="sd-issue"${i.open ? " open" : ""}><summary><span class="n">#${esc(i.number)}</span>
        <span>${esc(i.title || "(title not recorded)")}</span><span>${outcomeBadge(i.outcome)} <span class="kick">${dur(i.seconds)}${i.polls ? ` · ${fmtInt(i.polls)} polls` : ""}</span></span></summary>
      <div class="sd-steps">${steps || `<div class="sd-step">no events recorded</div>`}</div>
      ${(i.lessons ?? []).length ? `<div class="sd-note"><b>Lessons recorded during this issue:</b> ${i.lessons.map(esc).join(" · ")}</div>` : ""}
      ${i.applied ? `<div class="sd-note">${fmtInt(i.applied)} prior lesson(s) were rendered into this issue's prompts.</div>` : ""}
    </details>`;
  }).join("");
  const triage = (r.triage ?? []).map((t) => `<div class="feed-item"><b>#${esc(t.issue)}</b> — ${esc(t.placement)} <span class="when">${esc(t.at)}</span></div>`).join("");
  el("sd-detail").innerHTML = `
    ${params}
    <div class="feed-item"><b>Started</b> — ${esc(r.started_at)} · ran ${dur(r.seconds)}${r.pid ? ` · pid ${fmtInt(r.pid)}` : ""}${r.stop_reason ? ` · stopped: ${esc(r.stop_reason)}` : ""}</div>
    <div class="feed-item"><b>Work</b> — ${fmtInt(r.claimed)} claimed, ${fmtInt(r.changed)} changed, ${fmtInt(r.prs_opened)} PRs opened, ${fmtInt(r.prs_merged)} merged,
      ${fmtInt(r.verified)} verified locally, ${fmtInt(r.waived)} waived, ${fmtInt(r.deferred)} deferred, ${fmtInt(r.escalated)} escalated</div>
    <div class="feed-item"><b>Spend</b> — ${fmtUsd(r.spend_usd)} metered in this session's window · <b>Lessons</b> — ${fmtInt((r.lesson_rows ?? []).length)} recorded</div>
    ${triage ? `<div class="kick" style="margin-top:var(--sp1)">Triage</div>${triage}` : ""}
    <div class="kick" style="margin-top:var(--sp2)">Issues · ${(r.issues ?? []).length}</div>
    ${issues || `<div class="empty">This session claimed nothing.</div>`}`;
}

/* ── lessons ─────────────────────────────────────────────────────────────── */
function renderLessons(d) {
  const lessons = d.lessons ?? [];
  const t = d.totals ?? {};
  if (!lessons.length) {
    el("sd-lessons").innerHTML = `<div class="empty">No lessons recorded yet. A lesson is what the reflection pass keeps
      after a turn; it lands in <code>context.db</code> and is rendered into later prompts — that second step is what "applied" counts.</div>`;
    return;
  }
  el("sd-lessons").innerHTML = `<div class="sd-note" style="margin:0 0 var(--sp1)">${fmtInt(t.lessons)} lessons across ${fmtInt(t.merged)} merged fixes
    — ${num(t.merged) ? (num(t.lessons) / num(t.merged)).toFixed(2) : "—"} per merge; ${fmtInt(t.applied)} rendered into a later prompt.</div>` +
    lessons.map((l) => `<div class="sd-lesson"><div>${esc(l.text)}</div>
      <div class="meta">${esc(l.slug)} · ${esc(l.kind)}<br>${esc(String(l.recorded_at).slice(0, 16).replace("T", " "))}</div></div>`).join("");
}

/* ── load ────────────────────────────────────────────────────────────────── */
async function load(quiet) {
  let d;
  try { d = await api("/api/self-driving-sessions"); }
  catch (e) { if (!quiet) fail("Could not load the self-driving sessions.", e); return; }
  S.data = d ?? {};
  renderHero(S.data); renderLive(S.data); renderQueue(S.data); renderChart(S.data);
  renderFunnel(S.data); renderGantt(S.data); renderSessions(S.data); renderLessons(S.data);
  el("sd-updated").textContent = "updated " + new Date().toLocaleTimeString("en-US", { hour12: false });
  const ids = (S.data.sessions ?? []).map((s) => s.id);
  if (S.session && ids.includes(S.session)) openSession(S.session, true);
  else if (ids.length) openSession(ids[0], true);
}

document.addEventListener("click", (ev) => {
  const hit = ev.target.closest("#panel-self-driving [data-session]");
  if (hit && hit.dataset.session) {
    openSession(hit.dataset.session);
    if (!hit.closest("#sd-sessions")) el("sd-detail")?.scrollIntoView({ behavior: reduced ? "auto" : "smooth", block: "nearest" });
  }
});

window.SelfDriving = { load };
})();
