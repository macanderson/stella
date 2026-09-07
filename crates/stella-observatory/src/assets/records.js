/* The context-records drill-down: three views under one tab.

     #records              every record that steers this workspace, ranked by
                           what needs a decision first
     #records/<id>         one record in full — its verdicts, every use, the
                           turns it was rendered into, the company it keeps
     #records/<id>/source  the words as they are stored and as the model saw
                           them, with the command that would change them

   Served at /assets/records.js and loaded after index.html's inline script,
   so the page's helpers (esc, num, fmtInt, fmtTok, ago, agoUnix, api, fail,
   highlightCode, $) are in scope. Everything drawn here comes from
   /api/context-records and /api/context-record; nothing is fetched from
   anywhere else, and nothing here writes — the source view shows the
   `stella context` or `stella memory` command for a change and runs none of
   them, because the observatory has no mutation verb.

   The palette rules the page states at its top hold here unchanged: the
   identity gold appears on exactly one primary action per view and never on
   a state; a standing is a glyph beside a colour, never a colour alone; and
   motion is a 240ms reveal on a view's first paint, nothing that loops. */
(() => {
"use strict";

const S = {
  list: null, listKey: "", detail: new Map(),
  query: "", standing: "all", sort: "standing", painted: new Set(),
};
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
const el = (id) => document.getElementById(id);

/* ── styles ───────────────────────────────────────────────────────────────
   Scoped under #panel-records so nothing here can restyle another tab. */
const css = `
#panel-records .rec-head{margin-bottom:var(--sp3)}
#panel-records .rec-h1{font:600 var(--fs-lg)/1.2 var(--mono);letter-spacing:var(--tr-heading);margin:2px 0 var(--sp1)}
#panel-records .rec-lede{font:var(--fw-prose) var(--fs-base)/var(--lh-prose) var(--mono);color:var(--text-2);max-width:var(--measure);margin:0}
#panel-records .rec-lede b{color:var(--text);font-weight:600}
#panel-records .rec-kpis{grid-template-columns:repeat(6,minmax(0,1fr))}
#panel-records .tile .v.bad{color:var(--bad)}
#panel-records .tile .v.ok{color:var(--ok)}
#panel-records .rec-tools{display:flex;flex-wrap:wrap;gap:var(--sp1);align-items:center;margin-bottom:var(--sp2)}
#panel-records .rec-tools input{background:var(--raised);border:1px solid var(--control-edge);color:var(--text);
  font:var(--fs-sm)/1.4 var(--mono);padding:7px 10px;width:min(320px,100%);border-radius:var(--radius-sm)}
#panel-records .rec-tools input::placeholder{color:var(--text-3)}
#panel-records .rec-seg{display:inline-flex;flex-wrap:wrap;gap:1px;background:var(--control-edge);
  border:1px solid var(--control-edge);border-radius:var(--radius-sm);max-width:100%}
#panel-records .rec-seg button{background:var(--ground);border:0;color:var(--text-2);cursor:pointer;
  font:var(--fs-micro)/1.4 var(--mono);padding:7px 11px;flex:1 0 auto}
#panel-records .rec-seg button[aria-checked="true"]{background:var(--accent);color:var(--ink);font-weight:600}
#panel-records .rec-seg button .n{opacity:.7;margin-left:5px;font-variant-numeric:tabular-nums}
#panel-records .rec-tools select{background:var(--raised);border:1px solid var(--control-edge);color:var(--text);
  font:var(--fs-micro)/1.4 var(--mono);padding:7px 10px;border-radius:var(--radius-sm)}
#panel-records .rec-count{font:var(--fs-micro)/1.4 var(--mono);color:var(--text-3);margin-left:auto}

#panel-records .rec-card{display:grid;grid-template-columns:minmax(0,1fr) 340px;gap:var(--sp3);
  background:var(--surface);border:1px solid var(--hairline-strong);border-left:3px solid var(--hairline-strong);
  padding:var(--sp2) var(--sp3) var(--sp2) 21px;margin-bottom:var(--sp1);cursor:pointer;min-width:0;
  transition:background var(--dur-state) linear,border-color var(--dur-state) linear}
#panel-records .rec-card:hover{background:var(--accent-wash)}
#panel-records .rec-card:focus-within{outline:1px solid var(--control-edge);outline-offset:-1px}
#panel-records .rec-card.s-failing{border-left-color:var(--bad)}
#panel-records .rec-card.s-earning{border-left-color:var(--ok)}
#panel-records .rec-top{display:flex;flex-wrap:wrap;align-items:center;gap:6px;margin-bottom:6px}
#panel-records .rec-top .when{margin-left:auto;font:var(--fs-micro)/1.6 var(--mono);color:var(--text-3);white-space:nowrap}
#panel-records .rec-title{margin:0 0 4px}
#panel-records .rec-title a{font:600 var(--fs-md)/1.35 var(--mono);color:var(--text);text-decoration:none;letter-spacing:-.005em}
#panel-records .rec-title a:hover{text-decoration:underline}
#panel-records .rec-stmt{font:var(--fw-prose) var(--fs-base)/1.55 var(--mono);color:var(--text-2);max-width:var(--measure);
  display:-webkit-box;-webkit-line-clamp:3;-webkit-box-orient:vertical;overflow:hidden;margin:0}
#panel-records .rec-foot{display:flex;flex-wrap:wrap;align-items:center;gap:var(--sp1);margin-top:var(--sp1)}
#panel-records .rec-id{font:var(--fs-micro)/1.6 var(--mono);color:var(--text-3);user-select:all;
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:min(52ch,100%)}
#panel-records .rec-ghost{background:none;border:1px solid var(--control-edge);border-radius:var(--radius-sm);
  color:var(--text-2);font:var(--fs-micro)/1.4 var(--mono);padding:3px 9px;cursor:pointer;text-decoration:none;white-space:nowrap}
#panel-records .rec-ghost:hover{color:var(--text);border-color:var(--accent)}
#panel-records .rec-go{margin-left:auto;font:var(--fs-sm)/1.5 var(--mono);color:var(--mark)}
#panel-records .rec-card:hover .rec-go{text-decoration:underline}
#panel-records .rec-stats{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:var(--sp1) var(--sp1);align-content:start}
#panel-records .rec-stat .k{display:block;font:var(--fs-micro)/1.3 var(--mono);color:var(--text-3);white-space:nowrap}
#panel-records .rec-stat .v{display:block;font:var(--fw-metric) var(--fs-xl)/1.1 var(--mono);letter-spacing:-.02em;
  font-variant-numeric:tabular-nums;margin-top:2px}
#panel-records .rec-stat .v.zero{color:var(--text-3)}
#panel-records .rec-bar{grid-column:1/-1;display:flex;height:8px;background:var(--sunken);border:1px solid var(--hairline);overflow:hidden}
#panel-records .rec-bar > i{display:block;height:100%;transition:width .6s var(--ease-reveal)}
#panel-records .rec-bar .yes{background:var(--ok)}
#panel-records .rec-bar .neu{background:var(--c3)}
#panel-records .rec-bar .no{background:var(--bad)}
#panel-records .rec-sub{grid-column:1/-1;font:var(--fs-micro)/1.5 var(--mono);color:var(--text-3)}
#panel-records .rec-sub b{color:var(--text-2);font-weight:500}
@media(max-width:820px){
  #panel-records .rec-card{grid-template-columns:minmax(0,1fr);gap:var(--sp2);padding-right:var(--sp2)}
  #panel-records .rec-kpis{grid-template-columns:repeat(3,minmax(0,1fr))}
}
@media(max-width:560px){#panel-records .rec-kpis{grid-template-columns:repeat(2,minmax(0,1fr))}}

#panel-records .rec-crumbs{display:flex;flex-wrap:wrap;align-items:center;gap:6px;
  font:var(--fs-micro)/1.6 var(--mono);color:var(--text-3);margin-bottom:var(--sp2)}
#panel-records .rec-crumbs a{color:var(--text-2);text-decoration:none}
#panel-records .rec-crumbs a:hover{color:var(--text);text-decoration:underline}
#panel-records .rec-crumbs .sep{color:var(--hairline-strong)}
#panel-records .rec-dhead{border-bottom:1px solid var(--hairline-strong);padding-bottom:var(--sp3);margin-bottom:var(--sp3)}
#panel-records .rec-dhead .rec-h1{font-size:var(--fs-xl);line-height:1.2;max-width:40ch;margin-bottom:var(--sp2)}
#panel-records .rec-prose{font:var(--fw-prose) var(--fs-md)/var(--lh-prose) var(--mono);color:var(--text);
  max-width:var(--measure);white-space:pre-wrap;word-break:break-word;margin:0 0 var(--sp2)}
#panel-records .rec-meta{display:flex;flex-wrap:wrap;align-items:center;gap:6px var(--sp2);
  font:var(--fs-sm)/1.6 var(--mono);color:var(--text-3)}
#panel-records .rec-meta b{color:var(--text-2);font-weight:500}
#panel-records .rec-actions{display:flex;flex-wrap:wrap;gap:var(--sp1);align-items:center;margin-top:var(--sp2)}
#panel-records .rec-primary{background:var(--identity);border:1px solid var(--identity);color:var(--identity-ink);
  font:600 var(--fs-sm)/1.4 var(--mono);padding:7px 14px;text-decoration:none;border-radius:var(--radius-sm)}
#panel-records .rec-primary:hover{filter:brightness(1.06)}
#panel-records .rec-cols{display:grid;grid-template-columns:minmax(0,1fr) 320px;gap:var(--sp2);align-items:start}
#panel-records .rec-cols > .rec-main > .card{margin-bottom:var(--sp2)}
#panel-records .rec-rail > .card{margin-bottom:var(--sp2)}
@media(max-width:980px){#panel-records .rec-cols{grid-template-columns:minmax(0,1fr)}}
#panel-records .rec-metric{font:var(--fw-metric) var(--fs-metric)/1 var(--mono);letter-spacing:var(--tr-display);
  font-variant-numeric:tabular-nums;margin:6px 0 4px}
#panel-records .rec-metric small{font:400 var(--fs-md)/1 var(--mono);color:var(--text-3);letter-spacing:0;margin-left:4px}
#panel-records .rec-bigbar{height:14px;margin:var(--sp1) 0}
#panel-records .rec-legend{display:flex;flex-wrap:wrap;gap:var(--sp2);font:var(--fs-micro)/1.5 var(--mono);color:var(--text-2)}
#panel-records .rec-legend i{display:inline-block;width:9px;height:9px;margin-right:5px;vertical-align:-1px}
#panel-records .rec-note{font:var(--fw-prose) var(--fs-sm)/1.6 var(--mono);color:var(--text-2);max-width:var(--measure);margin:var(--sp1) 0 0}
#panel-records .rec-note b{color:var(--text);font-weight:600}
#panel-records .rec-kv{display:grid;grid-template-columns:auto minmax(0,1fr);gap:4px var(--sp2);font:var(--fs-sm)/1.6 var(--mono)}
#panel-records .rec-kv .k{color:var(--text-3)}
#panel-records .rec-kv .v{color:var(--text-2);word-break:break-word}
#panel-records .rec-kv .v.mono{user-select:all}
#panel-records .rec-turn{display:grid;grid-template-columns:auto minmax(0,1fr) auto;gap:var(--sp1) var(--sp2);align-items:center;
  padding:8px 0;border-bottom:1px solid var(--hairline);text-decoration:none;color:var(--text-2);font:var(--fs-sm)/1.5 var(--mono)}
#panel-records .rec-turn:last-child{border-bottom:0}
#panel-records .rec-turn:hover{background:var(--accent-wash)}
#panel-records .rec-turn .n{color:var(--mark);font-variant-numeric:tabular-nums;white-space:nowrap}
#panel-records .rec-turn .p{color:var(--text);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#panel-records .rec-turn .m{color:var(--text-3);font-size:var(--fs-micro);white-space:nowrap;text-align:right}
#panel-records .rec-rel{display:block;padding:7px 0;border-bottom:1px solid var(--hairline);text-decoration:none;
  font:var(--fs-sm)/1.5 var(--mono);color:var(--text-2)}
#panel-records .rec-rel:last-child{border-bottom:0}
#panel-records .rec-rel:hover{color:var(--text)}
#panel-records .rec-rel .t{display:block;color:var(--text);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#panel-records .rec-rel .m{font-size:var(--fs-micro);color:var(--text-3)}
#panel-records td.rec-wrap{white-space:normal;max-width:36ch}
#panel-records details.rec-why{font:var(--fs-micro)/1.6 var(--mono);color:var(--text-3);margin-top:2px}
#panel-records details.rec-why summary{cursor:pointer}
#panel-records details.rec-why div{color:var(--text-2);white-space:normal;max-width:60ch;padding:2px 0 4px}
#panel-records .rec-src pre.cfg{max-height:none;color:var(--text)}
#panel-records .rec-src pre.cfg.plain{white-space:pre-wrap;word-break:break-word;font:var(--fw-prose) var(--fs-base)/1.6 var(--mono)}
#panel-records .rec-cmd{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:6px var(--sp1);align-items:center;
  padding:var(--sp1) 0;border-bottom:1px solid var(--hairline)}
#panel-records .rec-cmd:last-child{border-bottom:0}
#panel-records .rec-cmd .why{grid-column:1/-1;font:var(--fs-micro)/1.5 var(--mono);color:var(--text-3)}
#panel-records .rec-cmd code{display:block;font:var(--fs-sm)/1.5 var(--mono);color:var(--text);white-space:pre-wrap;
  word-break:break-all;background:var(--sunken);border:1px solid var(--hairline);padding:6px 9px;user-select:all}
#panel-records .rec-readonly{font:var(--fs-micro)/1.6 var(--mono);color:var(--text-3);border-top:1px solid var(--hairline);
  padding-top:var(--sp1);margin-top:var(--sp1)}
#panel-records .rec-in{animation:rec-in var(--dur-reveal) var(--ease-reveal)}
@keyframes rec-in{from{opacity:0;transform:translateY(4px)}to{opacity:1;transform:none}}
@media(prefers-reduced-motion:reduce){#panel-records .rec-in{animation:none}#panel-records .rec-bar > i{transition:none}}
`;
const style = document.createElement("style");
style.textContent = css;
document.head.appendChild(style);

/* ── vocabulary ─────────────────────────────────────────────────────────── */
const STANDING = {
  failing:    { cls: "bad", glyph: "✕", label: "failing" },
  earning:    { cls: "ok",  glyph: "✓", label: "earning its place" },
  unassessed: { cls: "dim", glyph: "○", label: "unassessed" },
  unused:     { cls: "dim", glyph: "·", label: "never rendered" },
};
const VERDICT = {
  helpful:     { cls: "ok",  glyph: "✓", label: "helpful" },
  not_helpful: { cls: "bad", glyph: "✕", label: "not helpful" },
  neutral:     { cls: "dim", glyph: "○", label: "neutral" },
};
const PLANE = { recall: "recalled memory", published: "published record", missing: "no longer stored" };

const standingOf = (s) => STANDING[s] ?? STANDING.unassessed;
const badge = ({ cls, glyph, label }, title = "") =>
  `<span class="badge ${cls}"${title ? ` title="${esc(title)}"` : ""}>${glyph} ${esc(label)}</span>`;
const chip = (text, title = "") => text
  ? `<span class="chip"${title ? ` title="${esc(title)}"` : ""}>${esc(text)}</span>` : "";
const nz = (v) => `<span class="v${num(v) ? "" : " zero"}">${fmtInt(v)}</span>`;
const plural = (n, one, many = one + "s") => `${fmtInt(n)} ${num(n) === 1 ? one : many}`;
/* The chips that say what a record is, without saying it twice: a recalled
   memory's kind, plane and origin used to read "memory · recalled memory ·
   memory". One chip per distinct word. */
function identityChips(r) {
  const p = r.published ?? null;
  const words = [];
  const add = (text, title) => { if (text && !words.some((w) => w.text === text)) words.push({ text, title }); };
  add(r.kind, "kind");
  add(r.plane === "recall" ? "recalled" : PLANE[r.plane] ?? r.plane, "where it lives");
  add(r.origin, "origin");
  if (p?.steering_force) add(p.steering_force, "steering force");
  if (p?.enforcement_mode && p.enforcement_mode !== "none") add(`enforced: ${p.enforcement_mode}`);
  if (r.superseded) add("superseded");
  return words.map((w) => chip(w.text, w.title)).join("");
}
/* The statement with the part the title already shows removed, so a card
   never reads the same words twice in a row. A title is the first sentence,
   so the remainder starts at the second; a title the server clipped mid-
   sentence continues from the clip, on the next whole word. */
function remainder(r) {
  const title = String(r.title ?? ""), text = String(r.statement ?? "");
  if (!title || !text) return text;
  if (title.endsWith("…")) {
    const head = title.slice(0, -1);
    if (!text.startsWith(head)) return text;
    return "…" + text.slice(head.length).replace(/^\S*\s*/, "");
  }
  if (!text.startsWith(title)) return text;
  return text.slice(title.length).replace(/^[.!?]\s*/, "").trim();
}
const task = (id) => String(id ?? "").replace(/^session:/, "");
const when = (ts) => ts ? ago(ts) : "—";
const href = (id, sub = "") => `#records/${esc(id)}${sub ? "/" + sub : ""}`;

/* The assessment bar: helpful / neutral / not helpful over the assessed
   uses, the remainder of the track left empty for uses nothing has judged.
   Widths are shares of `uses`, so a record used thirty times and judged
   twice shows two thin marks on a long empty track, which is the truth. */
function bar(h, big = false) {
  const uses = Math.max(num(h.uses), 1);
  const seg = (n, cls, label) => num(n)
    ? `<i class="${cls}" style="width:${(100 * num(n) / uses).toFixed(2)}%" title="${esc(label)}"></i>` : "";
  return `<div class="rec-bar${big ? " rec-bigbar" : ""}" role="img"
      aria-label="${esc(`${fmtInt(h.helpful)} helpful, ${fmtInt(h.neutral)} neutral, ${fmtInt(h.not_helpful)} not helpful of ${fmtInt(h.uses)} uses`)}">
    ${seg(h.helpful, "yes", `${fmtInt(h.helpful)} helpful`)}${seg(h.neutral, "neu", `${fmtInt(h.neutral)} neutral`)}${seg(h.not_helpful, "no", `${fmtInt(h.not_helpful)} not helpful`)}
  </div>`;
}

/* One sentence on the standing, and what would move it — written from the
   fold's own numbers so it can never disagree with the badge beside it. */
function standingNote(rec, pol) {
  const h = rec.health ?? {};
  const min = num(pol.min_attributable_uses), thr = num(pol.not_helpful_ratio_threshold);
  const ratio = num(h.eligible_assessed) ? num(h.eligible_not_helpful) / num(h.eligible_assessed) : 0;
  switch (rec.standing) {
    case "failing":
      return `<b>Failing to earn its place.</b> ${fmtInt(h.eligible_not_helpful)} of ${fmtInt(h.eligible_assessed)}
        pruning-eligible verdicts say not helpful — ${pct(ratio)}, at or over the ${pct(thr)} threshold.
        Necessary for retirement, never sufficient: the protected-category check runs after this.`;
    case "earning":
      return `<b>Earning its place.</b> ${fmtInt(h.eligible_assessed)} pruning-eligible verdicts (the floor is ${fmtInt(min)})
        with ${pct(ratio)} not helpful, under the ${pct(thr)} threshold.`;
    case "unassessed":
      return `<b>Not yet assessed.</b> Rendered ${plural(h.uses, "time")} across ${plural(h.distinct_tasks, "task")},
        but only ${plural(h.eligible_assessed, "pruning-eligible verdict")} — it needs ${fmtInt(min)} before its ratio
        means anything. Agent self-reports inform this page but may not retire a record.`;
    default:
      return `<b>Never rendered.</b> Published, and in no prompt the ledger has seen. It steers only when its
        scope matches a turn.`;
  }
}

/* ── list ───────────────────────────────────────────────────────────────── */
function visibleRecords() {
  const q = S.query.trim().toLowerCase();
  let rows = (S.list?.records ?? []).filter((r) => S.standing === "all" || r.standing === S.standing);
  if (q) {
    rows = rows.filter((r) => [r.title, r.statement, r.id, r.kind, r.origin,
      ...(r.published?.tags ?? []), ...(r.domains ?? [])].join("\n").toLowerCase().includes(q));
  }
  const by = {
    uses:   (a, b) => num(b.health?.uses) - num(a.health?.uses),
    tokens: (a, b) => num(b.prompt_tokens) - num(a.prompt_tokens),
    newest: (a, b) => String(b.recorded_at).localeCompare(String(a.recorded_at)),
    recent: (a, b) => String(b.last_used ?? "").localeCompare(String(a.last_used ?? "")),
  }[S.sort];
  return by ? [...rows].sort(by) : rows;
}

function renderList() {
  const d = S.list ?? {};
  const t = d.totals ?? {};
  const rows = visibleRecords();
  const all = d.records ?? [];
  const count = (s) => all.filter((r) => r.standing === s).length;
  const first = !S.painted.has("list");
  S.painted.add("list");
  const lede = all.length
    ? `<b>${plural(t.records, "record")}</b> steer this workspace — ${fmtInt(t.uses)} renderings across
       ${plural(t.tasks, "task")}. ${num(t.failing)
         ? `<b>${plural(t.failing, "record")}</b> ${num(t.failing) === 1 ? "is" : "are"} failing to earn ${num(t.failing) === 1 ? "its" : "their"} place.`
         : "Nothing is failing."}
       Ranked by what needs a decision first.`
    : d.present
      ? `No record has been rendered into a prompt yet, and nothing is published under <code>.stella/rules</code>.`
      : `No <code>.stella/private/context.db</code> in this workspace yet — the ledger appears after the first session
         that recalls a memory or loads a published rule.`;
  const tile = (k, v, sub, cls = "") =>
    `<div class="card tile"><span class="k">${k}</span><span class="v${cls ? " " + cls : ""}">${fmtInt(v)}</span><span class="sub">${sub}</span></div>`;
  const seg = (key, label) =>
    `<button role="radio" aria-checked="${S.standing === key}" data-standing="${key}">${label}<span class="n">${
      key === "all" ? fmtInt(all.length) : fmtInt(count(key))}</span></button>`;

  el("rec-root").innerHTML = `
    <div class="rec-head${first ? " rec-in" : ""}">
      <div class="kick">Context records · is injected context earning its tokens?</div>
      <h1 class="rec-h1">Records</h1>
      <p class="rec-lede">${lede}</p>
    </div>
    <div class="grid kpis rec-kpis">
      ${tile("records", t.records, "recalled + published")}
      ${tile("failing", t.failing, "need a decision", num(t.failing) ? "bad" : "")}
      ${tile("earning", t.earning, "attributable, under threshold", num(t.earning) ? "ok" : "")}
      ${tile("unassessed", t.unassessed, "rendered, not judged")}
      ${tile("renderings", t.uses, "uses in the ledger")}
      ${tile("tasks", t.tasks, "distinct sessions")}
    </div>
    <div class="rec-tools">
      <input type="search" id="rec-q" placeholder="Search statements, ids, domains…" value="${esc(S.query)}"
        aria-label="Search records">
      <div class="rec-seg" role="radiogroup" aria-label="Standing">
        ${seg("all", "all")}${seg("failing", "✕ failing")}${seg("earning", "✓ earning")}${seg("unassessed", "○ unassessed")}${seg("unused", "· unused")}
      </div>
      <select id="rec-sort" aria-label="Sort">
        ${[["standing", "by standing"], ["uses", "most used"], ["recent", "most recently used"],
           ["tokens", "most prompt tokens"], ["newest", "newest"]]
          .map(([k, l]) => `<option value="${k}"${S.sort === k ? " selected" : ""}>${l}</option>`).join("")}
      </select>
      <span class="rec-count">${fmtInt(rows.length)} of ${fmtInt(all.length)}</span>
    </div>
    <div class="rec-list">${rows.length ? rows.map((r, i) => card(r, first ? i : -1)).join("")
      : `<div class="empty">${all.length ? "No record matches." : "Nothing to list yet."}</div>`}</div>`;

  const q = el("rec-q");
  q.addEventListener("input", () => { S.query = q.value; repaintList(); });
  el("rec-sort").addEventListener("change", (ev) => { S.sort = ev.target.value; repaintList(); });
}

/* Re-render only the list body and its count, so typing in the search box
   never rebuilds the box the reader is typing in. */
function repaintList() {
  const rows = visibleRecords();
  const all = S.list?.records ?? [];
  const list = el("rec-root")?.querySelector(".rec-list");
  if (!list) return;
  list.innerHTML = rows.length ? rows.map((r) => card(r, -1)).join("")
    : `<div class="empty">${all.length ? "No record matches." : "Nothing to list yet."}</div>`;
  const n = el("rec-root").querySelector(".rec-count");
  if (n) n.textContent = `${fmtInt(rows.length)} of ${fmtInt(all.length)}`;
  el("rec-root").querySelectorAll(".rec-seg button").forEach((b) =>
    b.setAttribute("aria-checked", String(b.dataset.standing === S.standing)));
}

function card(r, i) {
  const h = r.health ?? {};
  const p = r.published ?? null;
  const st = standingOf(r.standing);
  const delay = i >= 0 && !reduced ? ` style="animation-delay:${Math.min(i, 12) * 24}ms"` : "";
  const tags = p?.tags?.length ? p.tags : (r.domains ?? []);
  return `<article class="rec-card s-${esc(r.standing)}${i >= 0 ? " rec-in" : ""}" data-id="${esc(r.id)}"${delay}>
    <div class="rec-body">
      <div class="rec-top">
        ${badge(st)}${identityChips(r)}
        <span class="when" title="${esc(r.last_used ?? "")}">${r.last_used ? "used " + when(r.last_used) : "never used"}</span>
      </div>
      <h3 class="rec-title"><a href="${href(r.id)}">${esc(r.title || r.id)}</a></h3>
      ${remainder(r) ? `<p class="rec-stmt">${esc(remainder(r))}</p>` : ""}
      <div class="rec-foot">
        <code class="rec-id" title="${esc(r.id)}">${esc(r.id)}</code>
        <button class="rec-ghost" data-copy="${esc(r.id)}" title="Copy the id">copy</button>
        ${tags.length ? `<span class="kick">${esc(tags.slice(0, 4).join(" · "))}</span>` : ""}
        <span class="rec-go">open →</span>
      </div>
    </div>
    <div class="rec-stats" aria-label="Health">
      <div class="rec-stat"><span class="k">uses</span>${nz(h.uses)}</div>
      <div class="rec-stat"><span class="k">tasks</span>${nz(h.distinct_tasks)}</div>
      <div class="rec-stat"><span class="k">helpful</span>${nz(h.helpful)}</div>
      <div class="rec-stat"><span class="k">not helpful</span>${nz(h.not_helpful)}</div>
      ${bar(h)}
      <div class="rec-sub"><b>${fmtInt(h.assessed_uses)}</b> assessed · <b>${fmtTok(r.prompt_tokens)}</b> prompt tokens
        over <b>${fmtInt(r.rendered_turns)}</b> ${num(r.rendered_turns) === 1 ? "turn" : "turns"}</div>
    </div>
  </article>`;
}

/* ── record page ────────────────────────────────────────────────────────── */
function renderDetail(d, sub) {
  const r = d.record ?? {};
  const h = r.health ?? {};
  const p = r.published ?? null;
  const pol = d.policy ?? {};
  const st = standingOf(r.standing);
  const key = `${r.id}/${sub}`;
  const first = !S.painted.has(key);
  S.painted.add(key);
  const crumbs = `<div class="rec-crumbs">
      <a href="#records">Records</a><span class="sep">/</span>
      ${sub ? `<a href="${href(r.id)}">${esc(r.title || r.id)}</a><span class="sep">/</span><span>source</span>`
            : `<span>${esc(r.title || r.id)}</span>`}
    </div>`;
  const head = `<div class="rec-dhead${first ? " rec-in" : ""}">
      <div class="rec-top">
        ${badge(st)}${identityChips(r)}${p?.status ? chip(`status: ${p.status}`) : ""}
      </div>
      <h1 class="rec-h1">${esc(r.title || r.id)}</h1>
      ${sub ? "" : `<p class="rec-prose">${esc(r.content ?? r.statement ?? "")}</p>`}
      <div class="rec-meta">
        <span><code class="rec-id" title="${esc(r.id)}">${esc(r.id)}</code>
          <button class="rec-ghost" data-copy="${esc(r.id)}" title="Copy the id">copy</button></span>
        ${r.recorded_at ? `<span>recorded <b title="${esc(r.recorded_at)}">${when(r.recorded_at)}</b></span>` : ""}
        <span>last used <b title="${esc(r.last_used ?? "")}">${when(r.last_used)}</b></span>
        ${(r.domains ?? []).length ? `<span>domains <b>${esc(r.domains.join(" · "))}</b></span>` : ""}
        ${p?.tags?.length ? `<span>tags <b>${esc(p.tags.join(" · "))}</b></span>` : ""}
      </div>
      <div class="rec-actions">
        ${sub ? `<a class="rec-primary" href="${href(r.id)}">← Back to the record</a>`
              : `<a class="rec-primary" href="${href(r.id, "source")}">View source</a>`}
        <a class="rec-ghost" href="#records">All records</a>
        ${d.related?.length && !sub ? `<span class="kick">rendered beside ${plural(d.related.length, "other record")}</span>` : ""}
      </div>
    </div>`;
  el("rec-root").innerHTML = crumbs + head + (sub ? sourceBody(d) : detailBody(d, pol, st));
  if (!first) return;
  scrollTo({ top: 0, behavior: "auto" });
}

function detailBody(d, pol, st) {
  const r = d.record ?? {};
  const h = r.health ?? {};
  const p = r.published ?? null;
  const share = num(h.assessed_uses) ? num(h.helpful) / num(h.assessed_uses) : null;
  const uses = d.uses ?? [];
  const turns = d.renderings ?? [];
  const related = d.related ?? [];

  const verdictCell = (v) => {
    if (!v) return `<span class="badge dim">— no verdict</span>`;
    const vv = VERDICT[v.evaluation] ?? { cls: "dim", glyph: "○", label: String(v.evaluation ?? "") };
    const why = v.statement || (v.effects ?? []).length
      ? `<details class="rec-why"><summary>why</summary><div>${esc(v.statement ?? "")}${
          (v.effects ?? []).length ? `<div class="kick">${esc(v.effects.join(" · "))}</div>` : ""}</div></details>` : "";
    return `${badge(vv)} <span class="kick">${esc(v.method ?? "")}${v.method ? " · " : ""}${fmtInt(v.confidence)}% confident${
      v.had_opportunity === false ? " · no opportunity" : ""}</span>${why}`;
  };
  const usesTable = uses.length
    ? `<div class="scroll-x"><table><thead><tr>
        <th scope="col">when</th><th scope="col">task</th><th scope="col">how</th><th scope="col">verdict</th>
       </tr></thead><tbody>${uses.map((u) => `<tr>
        <td class="num" title="${esc(u.observed_at)}">${when(u.observed_at)}</td>
        <td class="main" title="${esc(u.task_id)}">${esc(task(u.task_id))}</td>
        <td>${esc(u.use_kind)}${u.influence_stage && u.influence_stage !== "none" ? ` · ${esc(u.influence_stage)}` : ""}</td>
        <td class="rec-wrap">${verdictCell(u.verdict)}</td></tr>`).join("")}</tbody></table></div>`
    : `<div class="empty">No use recorded.<br>A use lands here each time this record is rendered into a prompt.</div>`;

  const outcome = (o) => o === "completed" ? `<span class="badge ok">✓ completed</span>`
    : o ? `<span class="badge dim">○ ${esc(o)}</span>` : "";
  const turnRows = turns.length
    ? turns.map((t) => `<a class="rec-turn" href="#transcript/${esc(String(t.execution_id))}">
        <span class="n">#${fmtInt(t.execution_id)}</span>
        <span class="p" title="${esc(t.prompt ?? "")}">${esc(t.prompt ?? "(no prompt recorded)")}</span>
        <span class="m">${outcome(t.outcome)} · ${fmtTok(t.prompt_tokens)} tok · ${when(t.ts)}</span>
      </a>`).join("")
    : `<div class="empty">No turn in the store carries this record.<br>The store's context receipts are written by
       the session; the ledger above is folded from them after the turn.</div>`;

  const scope = p ? `<div class="card">
      <div class="kick">Published record · ${esc(p.path ? p.path.split("/").pop() : "")}</div>
      <h2>Scope</h2>
      <div class="rec-kv">
        <span class="k">handle</span><span class="v mono">^${esc(p.handle)}</span>
        <span class="k">force</span><span class="v">${esc(p.steering_force || "—")}</span>
        <span class="k">precedence</span><span class="v">${p.precedence != null ? fmtInt(p.precedence) : "—"}</span>
        <span class="k">enforcement</span><span class="v">${esc(p.enforcement_mode || "none")}</span>
        <span class="k">paths</span><span class="v">${esc((p.applies_to?.paths ?? []).join(", ") || "any")}</span>
        <span class="k">tasks</span><span class="v">${esc((p.applies_to?.tasks ?? []).join(", ") || "any")}</span>
        <span class="k">keywords</span><span class="v">${esc((p.applies_to?.keywords ?? []).join(", ") || "any")}</span>
        ${p.contributed_by ? `<span class="k">via</span><span class="v">${esc(p.contributed_by)} plugin</span>` : ""}
        ${p.record_id ? `<span class="k">record</span><span class="v mono">${esc(p.record_id)}</span>` : ""}
      </div>
    </div>`
    : `<div class="card">
      <div class="kick">Recalled memory · context.db</div>
      <h2>Recall</h2>
      <div class="rec-kv">
        <span class="k">kind</span><span class="v">${esc(r.kind)}</span>
        <span class="k">origin</span><span class="v">${esc(r.origin || "—")}</span>
        <span class="k">recorded</span><span class="v" title="${esc(r.recorded_at ?? "")}">${when(r.recorded_at)}</span>
        <span class="k">live</span><span class="v">${r.superseded ? "superseded — a newer revision serves" : "current revision"}</span>
      </div>
    </div>`;

  return `<div class="rec-cols">
    <div class="rec-main">
      <div class="card">
        <div class="kick">Every use, judged · helpful / neutral / not helpful over ${plural(h.uses, "rendering")}</div>
        <h2>Verdicts</h2>
        ${bar(h, true)}
        <div class="rec-legend">
          <span><i style="background:var(--ok)"></i>${fmtInt(h.helpful)} helpful</span>
          <span><i style="background:var(--c3)"></i>${fmtInt(h.neutral)} neutral</span>
          <span><i style="background:var(--bad)"></i>${fmtInt(h.not_helpful)} not helpful</span>
          <span><i style="background:var(--sunken);border:1px solid var(--hairline)"></i>${fmtInt(num(h.uses) - num(h.assessed_uses))} unjudged</span>
        </div>
        <p class="rec-note">${standingNote(r, pol)}</p>
      </div>
      <div class="card">
        <div class="kick">Newest first · ${plural(uses.length, "use")} listed</div>
        <h2>Uses</h2>
        ${usesTable}
      </div>
      <div class="card">
        <div class="kick">The turns this record was rendered into · open one to read the prompt it shaped</div>
        <h2>Rendered into</h2>
        ${turnRows}
      </div>
    </div>
    <aside class="rec-rail">
      <div class="card">
        <div class="kick">Helpful share of assessed uses</div>
        <h2>Health ${badge(st)}</h2>
        <div class="rec-metric">${share == null ? "—" : pct(share)}<small>${share == null ? "not assessed" : `of ${fmtInt(h.assessed_uses)}`}</small></div>
        <div class="rec-kv">
          <span class="k">uses</span><span class="v">${fmtInt(h.uses)}</span>
          <span class="k">tasks</span><span class="v">${fmtInt(h.distinct_tasks)}</span>
          <span class="k">eligible</span><span class="v">${fmtInt(h.eligible_assessed)} verdicts · ${fmtInt(h.eligible_not_helpful)} not helpful</span>
          <span class="k">threshold</span><span class="v">${pct(pol.not_helpful_ratio_threshold)} over ≥ ${fmtInt(pol.min_attributable_uses)}, at ≥ ${fmtInt(pol.min_attribution_confidence)}% confidence</span>
          <span class="k">prompt cost</span><span class="v">${fmtTok(r.prompt_tokens)} tokens over ${plural(r.rendered_turns, "turn")}</span>
        </div>
      </div>
      ${scope}
      <div class="card">
        <div class="kick">Rendered beside · shared turns</div>
        <h2>Company it keeps</h2>
        ${related.length ? related.map((x) => `<a class="rec-rel" href="${href(x.id)}">
            <span class="t" title="${esc(x.id)}">${esc(x.title || x.id)}</span>
            <span class="m">${esc(x.kind)} · ${plural(x.shared_turns, "shared turn")}</span></a>`).join("")
          : `<div class="empty">Rendered alone, or not yet.</div>`}
      </div>
    </aside>
  </div>`;
}

/* ── source view ────────────────────────────────────────────────────────── */
function sourceBody(d) {
  const r = d.record ?? {};
  const s = d.source ?? {};
  const cmds = s.commands ?? [];
  const isToml = s.language === "toml";
  const text = String(s.text ?? "");
  const memory = s.memory ?? null;
  const meta = s.kind === "published"
    ? `<div class="rec-kv">
        <span class="k">file</span><span class="v mono">${esc(s.path ?? "")}</span>
        <span class="k">lineage</span><span class="v mono">${esc(r.published?.lineage_id ?? "")}</span>
        ${r.published?.record_hash ? `<span class="k">hash</span><span class="v mono">${esc(r.published.record_hash)}</span>` : ""}
      </div>`
    : s.kind === "recall"
      ? `<div class="rec-kv">
        <span class="k">uri</span><span class="v mono">${esc(s.uri ?? "—")}</span>
        ${memory ? `<span class="k">memory</span><span class="v mono">${esc(memory.id)} · ${esc(memory.kind)}</span>
          <span class="k">revision</span><span class="v">${memory.superseded_at ? "superseded " + when(memory.superseded_at) : "current"} · recorded ${when(memory.recorded_at)}</span>` : ""}
        ${s.valid_from || s.valid_to ? `<span class="k">valid</span><span class="v">${esc(s.valid_from ?? "…")} → ${esc(s.valid_to ?? "open")}</span>` : ""}
      </div>`
      : `<div class="empty">Neither store holds this record any more — its uses remain in the ledger, its words do not.</div>`;
  return `<div class="rec-cols">
    <div class="rec-main">
      <div class="card rec-src">
        <div class="kick">${s.kind === "published" ? "The record set as published · every [[record]] in the file" : "The words as stored"}</div>
        <h2>${isToml ? "Source · TOML" : "Source"}</h2>
        ${text ? `<pre class="cfg${isToml ? "" : " plain"}">${isToml ? highlightCode(text) : esc(text)}</pre>`
               : `<div class="empty">No source text to show.</div>`}
      </div>
      <div class="card rec-src">
        <div class="kick">The block exactly as it reached the model · newest rendering</div>
        <h2>As the model saw it</h2>
        ${s.as_rendered ? `<pre class="cfg plain">${esc(s.as_rendered)}</pre>`
          : `<div class="empty">Not rendered into any turn the store still holds.</div>`}
      </div>
    </div>
    <aside class="rec-rail">
      <div class="card">
        <div class="kick">Where it lives</div>
        <h2>Identity</h2>
        ${meta}
      </div>
      <div class="card">
        <div class="kick">Run in a terminal at the workspace root</div>
        <h2>Change it</h2>
        ${cmds.length ? cmds.map((c) => `<div class="rec-cmd">
            <span class="why">${esc(c.why)}</span>
            <code>${esc(c.run)}</code>
            <button class="rec-ghost" data-copy="${esc(c.run)}" title="Copy the command">copy</button>
          </div>`).join("") : `<div class="empty">Nothing to change.</div>`}
        <div class="rec-readonly">The Observatory is read-only by construction: it opens every store read-only and answers
          nothing but GET, so a change is a command you run, and the page reflects it on the next load.</div>
      </div>
    </aside>
  </div>`;
}

/* ── clipboard ──────────────────────────────────────────────────────────── */
async function copy(text, button) {
  const was = button.textContent;
  try {
    await navigator.clipboard.writeText(text);
    button.textContent = "copied";
  } catch {
    button.textContent = "select & copy";
  }
  setTimeout(() => { button.textContent = was; }, 1400);
}

document.addEventListener("click", (ev) => {
  const copyBtn = ev.target.closest("#panel-records [data-copy]");
  if (copyBtn) { ev.preventDefault(); ev.stopPropagation(); copy(copyBtn.dataset.copy, copyBtn); return; }
  const seg = ev.target.closest("#panel-records .rec-seg button");
  if (seg) { S.standing = seg.dataset.standing; repaintList(); return; }
  // The whole card opens the record; a click on a link or a selection inside
  // it is left to do what it does.
  const cardEl = ev.target.closest("#panel-records .rec-card");
  if (cardEl && !ev.target.closest("a, button") && !getSelection()?.toString()) {
    location.hash = `records/${cardEl.dataset.id}`;
  }
});

/* ── routing ────────────────────────────────────────────────────────────── */
const ID_RE = /^[\^A-Za-z0-9_.:@-]{1,200}$/;

/* Called by the page's loadTab with the route's argument: "" for the list,
   "<id>" for a record, "<id>/source" for its source. */
async function route(arg, force) {
  const raw = String(arg ?? "");
  const cut = raw.indexOf("/");
  const id = cut < 0 ? raw : raw.slice(0, cut);
  const sub = cut < 0 ? "" : raw.slice(cut + 1);
  if (!id) return loadList(force);
  if (!ID_RE.test(id) || (sub && sub !== "source")) { location.hash = "records"; return; }
  return loadDetail(id, sub, force);
}

async function loadList(force) {
  const key = state.project;
  if (force || !S.list || S.listKey !== key) {
    try { S.list = await api("/api/context-records") ?? {}; S.listKey = key; }
    catch (e) { fail("Could not load the context records.", e); return; }
  }
  renderList();
}

async function loadDetail(id, sub, force) {
  const key = `${state.project}\n${id}`;
  let d = force ? null : S.detail.get(key);
  if (!d) {
    try { d = await api(`/api/context-record?id=${encodeURIComponent(id)}`) ?? {}; }
    catch (e) { fail("Could not load that record.", e); return; }
    S.detail.set(key, d);
  }
  if (!d.found) {
    el("rec-root").innerHTML = `<div class="rec-crumbs"><a href="#records">Records</a><span class="sep">/</span><span>${esc(id)}</span></div>
      <div class="empty">No record named <code>${esc(id)}</code> in this workspace.<br>
      It may have been forgotten, or its file removed, since the link was made.
      <div style="margin-top:var(--sp2)"><a class="rec-ghost" href="#records">All records</a></div></div>`;
    return;
  }
  renderDetail(d, sub);
}

window.Records = { route };
/* The page routes to `location.hash` from its own inline script, which runs
   before this file has loaded — so a reload on `#records/…` reaches loadTab
   while `window.Records` is still undefined and draws nothing. Catching up
   here is what makes the address reloadable, which is the whole point of
   giving these views one. */
if (state.tab === "records") route(state.recordsArg, false);
})();
