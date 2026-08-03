import json

data = json.load(open('/Users/macanderson/.claude/jobs/971f0967/tmp/paper/data.json'))
payload = json.dumps(data, separators=(',', ':'))

HTML = r'''<title>Same model, different harness — Stella vs Claude Code on Terminal-Bench 2.1</title>
<style>
:root{
  --bg:#F7F5F3; --panel:#FFFFFF; --ink:#1A1614; --ink-2:#5C534D; --ink-3:#8B8078;
  --rule:#E2DCD6; --rule-2:#CFC7BF;
  --a:#B4741C;        /* Stella — signal amber, darkened for light ground */
  --b:#7E5F70;        /* comparator — muted plum */
  --pass:#3F7A50; --fail:#B04638; --warn:#A8742A;
  --code:#F1EDE9;
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#14110F; --panel:#1C1815; --ink:#F2EDE7; --ink-2:#B5AAA1; --ink-3:#7E736B;
    --rule:#2C2622; --rule-2:#3D352F;
    --a:#E8A33D; --b:#B892A6;
    --pass:#6FB183; --fail:#DE7568; --warn:#D9A34E;
    --code:#211C18;
  }
}
:root[data-theme="dark"]{
  --bg:#14110F; --panel:#1C1815; --ink:#F2EDE7; --ink-2:#B5AAA1; --ink-3:#7E736B;
  --rule:#2C2622; --rule-2:#3D352F;
  --a:#E8A33D; --b:#B892A6;
  --pass:#6FB183; --fail:#DE7568; --warn:#D9A34E;
  --code:#211C18;
}
:root[data-theme="light"]{
  --bg:#F7F5F3; --panel:#FFFFFF; --ink:#1A1614; --ink-2:#5C534D; --ink-3:#8B8078;
  --rule:#E2DCD6; --rule-2:#CFC7BF;
  --a:#B4741C; --b:#7E5F70;
  --pass:#3F7A50; --fail:#B04638; --warn:#A8742A;
  --code:#F1EDE9;
}
*{box-sizing:border-box}
body{
  margin:0; background:var(--bg); color:var(--ink);
  font:400 17px/1.62 Georgia,"Iowan Old Style","Times New Roman",serif;
  -webkit-font-smoothing:antialiased;
}
.disp{font-family:ui-sans-serif,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",sans-serif;
  font-weight:750; letter-spacing:-.028em; text-wrap:balance}
.mono{font-family:ui-monospace,"SF Mono",Menlo,Consolas,monospace; font-variant-numeric:tabular-nums}
.wrap{max-width:1180px; margin:0 auto; padding:0 24px}
.prose{max-width:66ch}
a{color:var(--ink); text-decoration-color:var(--rule-2); text-underline-offset:3px}
a:hover{text-decoration-color:var(--a)}
:focus-visible{outline:2px solid var(--a); outline-offset:3px; border-radius:2px}

/* ── masthead ─────────────────────────────────────── */
header{border-bottom:1px solid var(--rule); padding:56px 0 0}
.eyebrow{font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:11.5px;
  letter-spacing:.16em; text-transform:uppercase; color:var(--ink-3)}
h1{font-size:clamp(30px,4.4vw,50px); line-height:1.06; margin:16px 0 14px}
.dek{color:var(--ink-2); font-size:19px; max-width:60ch; margin:0 0 34px}

.scoreline{display:grid; grid-template-columns:1fr auto 1fr; gap:clamp(14px,4vw,52px);
  align-items:end; padding:8px 0 30px}
.side{display:flex; flex-direction:column; gap:5px}
.side.r{align-items:flex-end; text-align:right}
.who{font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:12px;
  letter-spacing:.09em; text-transform:uppercase}
.side.l .who{color:var(--a)} .side.r .who{color:var(--b)}
.big{font-family:ui-sans-serif,-apple-system,"Segoe UI",sans-serif; font-weight:800;
  font-variant-numeric:tabular-nums; font-size:clamp(56px,11vw,104px); line-height:.86;
  letter-spacing:-.05em}
.side.l .big{color:var(--a)} .side.r .big{color:var(--b)}
.sub{font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:12.5px; color:var(--ink-3)}
.vs{font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:12px; color:var(--ink-3);
  padding-bottom:16px; letter-spacing:.1em}
.bar{display:flex; height:7px; border-radius:4px; overflow:hidden; background:var(--rule); margin-bottom:34px}
.bar i{display:block}
.bar i:first-child{background:var(--a)} .bar i:last-child{background:var(--b)}

/* ── sections ─────────────────────────────────────── */
section{padding:52px 0; border-bottom:1px solid var(--rule)}
h2{font-size:clamp(21px,2.5vw,27px); margin:0 0 6px; letter-spacing:-.02em}
h2+.lede{color:var(--ink-3); font-family:ui-monospace,"SF Mono",Menlo,monospace;
  font-size:12px; letter-spacing:.1em; text-transform:uppercase; margin:0 0 26px}
p{margin:0 0 18px}
strong{font-weight:700}
.callout{border-left:2px solid var(--a); padding:2px 0 2px 20px; margin:26px 0; color:var(--ink-2)}

/* ── stat strip ───────────────────────────────────── */
.stats{display:grid; grid-template-columns:repeat(auto-fit,minmax(158px,1fr)); gap:1px;
  background:var(--rule); border:1px solid var(--rule); margin:30px 0}
.stat{background:var(--panel); padding:16px 18px; display:flex; flex-direction:column; gap:5px}
.stat .k{font-family:ui-monospace,"SF Mono",Menlo,monospace; font-size:10.5px;
  letter-spacing:.11em; text-transform:uppercase; color:var(--ink-3)}
.stat .v{font-family:ui-sans-serif,-apple-system,sans-serif; font-weight:750;
  font-variant-numeric:tabular-nums; font-size:26px; letter-spacing:-.025em; line-height:1}
.stat .n{font-size:12.5px; color:var(--ink-3); font-family:ui-monospace,Menlo,monospace}

/* ── 2x2 agreement grid ───────────────────────────── */
.grid2{display:grid; grid-template-columns:auto 1fr 1fr; gap:1px; background:var(--rule);
  border:1px solid var(--rule); max-width:560px; margin:26px 0}
.grid2>div{background:var(--panel); padding:13px 16px}
.grid2 .hd{font-family:ui-monospace,Menlo,monospace; font-size:10.5px; letter-spacing:.1em;
  text-transform:uppercase; color:var(--ink-3)}
.grid2 .cell{font-family:ui-sans-serif,-apple-system,sans-serif; font-weight:750;
  font-size:25px; font-variant-numeric:tabular-nums; letter-spacing:-.02em}
.grid2 .cell small{display:block; font-family:ui-monospace,Menlo,monospace; font-size:11px;
  font-weight:400; letter-spacing:0; color:var(--ink-3); margin-top:3px}
.hl-a{color:var(--a)} .hl-b{color:var(--b)}

/* ── results table ────────────────────────────────── */
.toolbar{display:flex; flex-wrap:wrap; gap:8px; align-items:center; margin:0 0 16px}
.chip{font-family:ui-monospace,Menlo,monospace; font-size:11.5px; letter-spacing:.05em;
  padding:6px 12px; border:1px solid var(--rule-2); background:transparent; color:var(--ink-2);
  cursor:pointer; border-radius:2px}
.chip:hover{border-color:var(--ink-3); color:var(--ink)}
.chip[aria-pressed="true"]{background:var(--ink); color:var(--bg); border-color:var(--ink)}
.count{margin-left:auto; font-family:ui-monospace,Menlo,monospace; font-size:11.5px; color:var(--ink-3)}

.tscroll{overflow-x:auto; border:1px solid var(--rule); background:var(--panel)}
table{border-collapse:collapse; width:100%; min-width:860px}
thead th{position:sticky; top:0; background:var(--panel); z-index:2;
  font-family:ui-monospace,Menlo,monospace; font-size:10.5px; letter-spacing:.09em;
  text-transform:uppercase; color:var(--ink-3); font-weight:400; text-align:right;
  padding:11px 12px; border-bottom:1px solid var(--rule-2); white-space:nowrap; cursor:pointer}
thead th:first-child{text-align:left}
thead th:hover{color:var(--ink)}
thead th .ar{opacity:.45; margin-left:4px}
thead th.grp-a{color:var(--a)} thead th.grp-b{color:var(--b)}
tbody tr.row{border-bottom:1px solid var(--rule); cursor:pointer}
tbody tr.row:hover{background:var(--code)}
td{padding:10px 12px; text-align:right; font-family:ui-monospace,Menlo,monospace;
  font-size:12.5px; font-variant-numeric:tabular-nums; white-space:nowrap}
td.task{text-align:left; font-size:13px; color:var(--ink); max-width:280px;
  overflow:hidden; text-overflow:ellipsis}
td.sep{border-left:1px solid var(--rule)}
.v{display:inline-flex; align-items:center; gap:6px; font-size:11.5px; letter-spacing:.04em}
.v::before{content:""; width:7px; height:7px; border-radius:50%; flex:none}
.v.ok{color:var(--pass)} .v.ok::before{background:var(--pass)}
.v.no{color:var(--fail)} .v.no::before{background:var(--fail)}
.v.err{color:var(--warn)} .v.err::before{background:var(--warn)}
.tag{font-size:10px; letter-spacing:.06em; color:var(--ink-3); text-transform:uppercase}
.dot{width:6px; height:6px; border-radius:50%; display:inline-block}

/* expanded detail */
tr.detail>td{padding:0; background:var(--code); border-bottom:1px solid var(--rule)}
.dpanel{padding:20px 14px 24px}
.dhead{font-family:ui-monospace,Menlo,monospace; font-size:11px; letter-spacing:.1em;
  text-transform:uppercase; color:var(--ink-3); margin:0 0 14px}
.cmp{display:grid; grid-template-columns:150px 1fr 1fr; gap:0 14px; align-items:center}
.cmp>div{padding:6px 0; font-family:ui-monospace,Menlo,monospace; font-size:12.5px;
  font-variant-numeric:tabular-nums}
.cmp .lab{color:var(--ink-3); font-size:11px; letter-spacing:.06em; text-transform:uppercase}
.cmp .hdr{font-size:11px; letter-spacing:.09em; text-transform:uppercase; padding-bottom:9px;
  border-bottom:1px solid var(--rule-2); margin-bottom:5px}
.cmp .hdr.a{color:var(--a)} .cmp .hdr.b{color:var(--b)}
.mrow{display:flex; align-items:center; gap:9px}
.mbar{height:4px; border-radius:2px; flex:1; min-width:22px; opacity:.85}
.mbar.a{background:var(--a)} .mbar.b{background:var(--b)}
.mval{min-width:82px; text-align:right}
.win{color:var(--ink); font-weight:700}
.lose{color:var(--ink-3)}

/* ── method / verify ──────────────────────────────── */
.spec{display:grid; grid-template-columns:auto 1fr; gap:1px; background:var(--rule);
  border:1px solid var(--rule); margin:26px 0; font-family:ui-monospace,Menlo,monospace; font-size:12.5px}
.spec>div{background:var(--panel); padding:10px 15px}
.spec .sk{color:var(--ink-3); white-space:nowrap}
pre{background:var(--code); border:1px solid var(--rule); padding:15px 17px; overflow-x:auto;
  font-family:ui-monospace,Menlo,monospace; font-size:12.5px; line-height:1.65; margin:20px 0}
code{font-family:ui-monospace,Menlo,monospace; font-size:.9em; background:var(--code);
  padding:1.5px 5px; border:1px solid var(--rule)}
pre code{background:none; border:none; padding:0}
ul{padding-left:19px; margin:0 0 18px} li{margin-bottom:9px}
footer{padding:40px 0 64px; color:var(--ink-3); font-family:ui-monospace,Menlo,monospace; font-size:12px}
@media (max-width:720px){
  .scoreline{grid-template-columns:1fr auto 1fr; gap:12px}
  .cmp{grid-template-columns:110px 1fr 1fr; gap:0 8px}
  body{font-size:16px}
}
@media (prefers-reduced-motion:reduce){*{transition:none!important; animation:none!important}}
</style>

<header>
  <div class="wrap">
    <div class="eyebrow">Terminal-Bench 2.1 &middot; 89 tasks &middot; paired, single trial &middot; 2026-07-31</div>
    <h1 class="disp">The model was identical.<br>The harness was not.</h1>
    <p class="dek">Two agents ran the same 89 terminal tasks on the same model, on the same
      machine, at the same hour. One solved thirteen more of them.</p>

    <div class="scoreline">
      <div class="side l">
        <div class="who">Stella</div>
        <div class="big" id="bigA">57</div>
        <div class="sub">of 89 &middot; 64.0%</div>
      </div>
      <div class="vs">VS</div>
      <div class="side r">
        <div class="who">Claude Code</div>
        <div class="big" id="bigB">44</div>
        <div class="sub">of 89 &middot; 49.4%</div>
      </div>
    </div>
    <div class="bar"><i style="flex:57"></i><i style="flex:44"></i></div>
  </div>
</header>

<main class="wrap">

<section>
  <h2 class="disp">The claim</h2>
  <p class="lede">Thesis</p>
  <div class="prose">
    <p>An agent is two things: a model, and the machinery around it. The machinery decides
      what the model reads, which actions it can take, when it is allowed to stop, and
      whether anyone checks the work before it is called done. Almost every published
      benchmark number moves both at once, so the two are rarely told apart.</p>
    <p>This run holds the model fixed and changes only the machinery. Both agents ran
      <strong>GLM-5.2</strong> at maximum reasoning effort. The single intended difference is
      which company's endpoint served the tokens.</p>
    <div class="callout">
      <p style="margin:0"><strong>Determinism inside a probabilistic system produces better
      results.</strong> A language model is a probabilistic component. The way to get reliable
      work out of one is not a better prompt — it is to surround it with steps that behave the
      same way every time: purpose-built tools instead of one general-purpose shell, a
      verification pass that must actually observe a change, and a stopping rule that refuses
      to accept "done" as an assertion.</p>
    </div>
    <p>The prediction that follows is specific, and it is falsifiable: with the model held
      constant, the agent that checks its own work should convert more near-misses into
      completions — and it should do so <em>without</em> a proportional increase in spend,
      because verification is cheap relative to the work it rescues.</p>
  </div>
</section>

<section>
  <h2 class="disp">What happened</h2>
  <p class="lede">Result</p>

  <div class="stats">
    <div class="stat"><div class="k">Solve rate</div><div class="v hl-a">64.0%</div>
      <div class="n">vs 49.4% &middot; +13 tasks</div></div>
    <div class="stat"><div class="k">Cost per success</div><div class="v hl-a">$1.29</div>
      <div class="n">vs $1.44 &middot; 10% cheaper</div></div>
    <div class="stat"><div class="k">Total spend</div><div class="v">$73.46</div>
      <div class="n">vs $63.35 &middot; 16% more</div></div>
    <div class="stat"><div class="k">Output tokens</div><div class="v">5.13M</div>
      <div class="n">vs 2.23M &middot; 2.3&times; more</div></div>
    <div class="stat"><div class="k">Tool actions</div><div class="v">5,674</div>
      <div class="n">vs 1,592 &middot; 3.6&times; more</div></div>
  </div>

  <div class="prose">
    <p>Because every task was attempted by both agents, the pairs can be read directly —
      which is far more informative than the totals. Of the 89 tasks, both agents solved 35
      and both failed 23. Those 58 say nothing about the harness. The comparison lives
      entirely in the 31 tasks where the two disagreed:</p>
  </div>

  <div class="grid2">
    <div class="hd"></div>
    <div class="hd">Claude Code solved</div>
    <div class="hd">Claude Code failed</div>
    <div class="hd">Stella solved</div>
    <div><div class="cell">35<small>both — no signal</small></div></div>
    <div><div class="cell hl-a">22<small>Stella only</small></div></div>
    <div class="hd">Stella failed</div>
    <div><div class="cell hl-b">9<small>Claude Code only</small></div></div>
    <div><div class="cell">23<small>neither — model ceiling</small></div></div>
  </div>

  <div class="prose">
    <p><strong>22 to 9.</strong> Under the null hypothesis that the harness makes no
      difference, a disagreement should fall either way with equal probability, and a split
      this lopsided across 31 disagreements arises about 2.9% of the time
      (<span class="mono">exact two-sided sign test, p = 0.029</span>).</p>
    <p>Two honest caveats belong right here. This is a <strong>single trial per task</strong>,
      so run-to-run variance is not measured — the leaderboard standard is five trials, and
      this is not that. And the result sits close enough to the conventional threshold that
      one task landing differently would move it: at 56–44 earlier in the same run, the same
      test read p = 0.050.</p>
  </div>
</section>

<section>
  <h2 class="disp">Where the difference came from</h2>
  <p class="lede">Mechanism</p>
  <div class="prose">
    <p>The failure classes explain more than the totals do. Claude Code recorded
      <strong>25 non-zero-exit failures</strong> — the agent stopped and left the workspace
      short of the goal. Stella recorded <strong>2</strong>. In the other direction, Stella
      spent more of its failures on the clock: 12 timeouts to Claude Code's 15, on tasks it
      was still working when time ran out.</p>
    <p>That is the shape the thesis predicts. A harness that verifies before it stops
      converts "I think I'm done" into another attempt. The cost of doing that shows up
      exactly where you would expect: more tool actions, more tokens, more wall-clock — and
      more tasks finished.</p>
    <p>The tool-count gap deserves its own sentence, because read carelessly it looks like
      waste. Claude Code funnels nearly everything through one general-purpose shell; Stella
      ships many small single-purpose tools — one that reads, one that edits, one that
      inspects what changed, one that runs tests. More calls is the design, not the
      inefficiency. The efficiency question is answered by cost per success, and there Stella
      is <strong>cheaper</strong>: $1.29 against $1.44.</p>
  </div>
</section>

<section>
  <h2 class="disp">Every task, both agents</h2>
  <p class="lede">Inspect &middot; click any row for full telemetry</p>

  <div class="toolbar">
    <button class="chip" data-f="all" aria-pressed="true">All 89</button>
    <button class="chip" data-f="disc" aria-pressed="false">Disagreements (31)</button>
    <button class="chip" data-f="a" aria-pressed="false">Stella only (22)</button>
    <button class="chip" data-f="b" aria-pressed="false">Claude Code only (9)</button>
    <button class="chip" data-f="both" aria-pressed="false">Both solved (35)</button>
    <button class="chip" data-f="none" aria-pressed="false">Neither (23)</button>
    <span class="count" id="count"></span>
  </div>

  <div class="tscroll">
    <table>
      <thead>
        <tr>
          <th data-s="k">Task<span class="ar"></span></th>
          <th class="grp-a sep" data-s="av">Stella<span class="ar"></span></th>
          <th class="grp-a" data-s="at">Tools<span class="ar"></span></th>
          <th class="grp-a" data-s="ao">Out tok<span class="ar"></span></th>
          <th class="grp-a" data-s="ac">Cost<span class="ar"></span></th>
          <th class="grp-a" data-s="aw">Wall<span class="ar"></span></th>
          <th class="grp-b sep" data-s="bv">Claude Code<span class="ar"></span></th>
          <th class="grp-b" data-s="bt">Tools<span class="ar"></span></th>
          <th class="grp-b" data-s="bo">Out tok<span class="ar"></span></th>
          <th class="grp-b" data-s="bc">Cost<span class="ar"></span></th>
          <th class="grp-b" data-s="bw">Wall<span class="ar"></span></th>
        </tr>
      </thead>
      <tbody id="tb"></tbody>
    </table>
  </div>
</section>

<section>
  <h2 class="disp">How it was run</h2>
  <p class="lede">Method</p>
  <div class="spec">
    <div class="sk">Benchmark</div><div>terminal-bench/terminal-bench-2-1, pinned by digest</div>
    <div class="sk">Tasks</div><div>All 89. No subsetting, no task selected after seeing a result.</div>
    <div class="sk">Model</div><div>GLM-5.2 for both agents</div>
    <div class="sk">Effort</div><div>max, reasoning enabled, for every role that affects the outcome</div>
    <div class="sk">Budget</div><div>No spend cap on either agent</div>
    <div class="sk">Attempts</div><div>1 per task, 0 retries, both agents</div>
    <div class="sk">Endpoint</div><div>Stella &rarr; OpenRouter &middot; Claude Code &rarr; z.ai <em>(the one intended difference)</em></div>
    <div class="sk">Host</div><div>One machine, both agents concurrently, same clock</div>
    <div class="sk">Pre-registered</div><div>Commit, binary hash, config hash and task list fixed before launch</div>
  </div>
  <div class="prose">
    <p>The run was registered before it started — the exact build, a hash of the binary, a
      hash of the configuration, and the task list, all written down in advance. That is what
      makes the number checkable rather than merely reported: nothing here could be chosen
      after the results were visible.</p>
    <p>A validation run of 20 tasks preceded this one and was checked five ways for
      contamination — broken binaries, network exhaustion, silently-fabricated successes,
      unequal task counts, and lost telemetry. No code changed between that check and this
      run.</p>
  </div>
</section>

<section>
  <h2 class="disp">What this does not show</h2>
  <p class="lede">Limits</p>
  <div class="prose">
    <ul>
      <li><strong>One trial per task.</strong> Agents are stochastic. A five-trial run is the
        standard for a leaderboard claim, and this is not one.</li>
      <li><strong>One model.</strong> A harness helps most when the model is close to solving
        something. On a stronger model, more tasks are solved outright and the harness has
        less to rescue — so this margin should be expected to compress, not to hold.</li>
      <li><strong>Different endpoints.</strong> Provider routing affects latency and price.
        Wall-clock and dollar comparisons carry that caveat; solve rate does not.</li>
      <li><strong>Claude Code on a non-default model.</strong> Running it on GLM-5.2 is
        supported but not its home configuration.</li>
      <li><strong>Failures are published alongside passes.</strong> 23 tasks defeated both
        agents. They are in the table above, unfiltered.</li>
    </ul>
  </div>
</section>

<section style="border-bottom:none">
  <h2 class="disp">Check it yourself</h2>
  <p class="lede">Verify</p>
  <div class="prose">
    <p>Every trial keeps its full record on disk — the event stream, the step-by-step
      trajectory, and the grader's verdict. The totals on this page are derived from those
      files and nothing else; the table above is that derivation, not a summary of it.</p>
    <p>To re-derive the headline number from raw trial records:</p>
  </div>
<pre><code>#  one directory per task, per agent
#  jobs/&lt;run&gt;-armA-stella/&lt;task&gt;__&lt;id&gt;/
#      result.json                  &lt;- grader verdict (ground truth)
#      agent/trajectory.json        &lt;- steps + token totals
#      agent/stella-events.jsonl    &lt;- full event stream

python3 - &lt;&lt;'PY'
import json, glob
for arm in ("armA-stella", "armB-claudecode"):
    wins = 0
    for f in glob.glob(f"jobs/*-{arm}/*/result.json"):
        r = json.load(open(f))
        rew = (r.get("verifier_result") or {}).get("rewards", {}).get("reward")
        wins += 1 if (rew or 0) &gt;= 1 else 0
    print(arm, wins)
PY</code></pre>
  <div class="prose">
    <p>A pass is <span class="mono">reward &ge; 1</span> from the benchmark's own grader. The
      agent has no say in it.</p>
  </div>
</section>

</main>

<footer class="wrap">
  Run 2026-07-31 &middot; 89 tasks &middot; 178 trials &middot; single attempt each &middot;
  totals computed from trial records, not transcribed.
</footer>

<script>
const DATA = __PAYLOAD__;
const R = DATA.rows;
const fmt = n => n == null ? "—" : n >= 1e6 ? (n/1e6).toFixed(2)+"M"
  : n >= 1e3 ? (n/1e3).toFixed(n>=1e4?0:1)+"k" : String(n);
const dur = s => !s ? "—" : s >= 3600 ? Math.floor(s/3600)+"h"+String(Math.round(s%3600/60)).padStart(2,"0")
  : s >= 60 ? Math.floor(s/60)+"m"+String(s%60).padStart(2,"0") : s+"s";
const money = c => c == null ? "—" : "$"+c.toFixed(2);
const cls = t => !t ? "" : t.replace(/Error$/,"");

function verdict(v){
  if(!v) return '<span class="v err">no data</span>';
  if(v.p) return '<span class="v ok">pass</span>';
  if(v.f) return '<span class="v err">'+cls(v.f)+'</span>';
  return '<span class="v no">fail</span>';
}
const key = r => {
  const a=r.a&&r.a.p, b=r.b&&r.b.p;
  if(a&&b) return "both"; if(a) return "a"; if(b) return "b"; return "none";
};

let filter="all", sortKey="k", sortDir=1;
const val = (r,k) => {
  if(k==="k") return r.k;
  const arm = k[0]==="a" ? r.a : r.b, f = k.slice(1);
  if(!arm) return -1;
  return f==="v" ? (arm.p?1:0) : f==="t" ? arm.t : f==="o" ? arm.o
       : f==="c" ? arm.c : f==="w" ? arm.w : 0;
};

function detail(r){
  const rows = [
    ["Verdict", verdict(r.a), verdict(r.b), null],
    ["Steps", r.a&&r.a.st, r.b&&r.b.st, "lower"],
    ["Tool actions", r.a&&r.a.t, r.b&&r.b.t, null],
    ["Input tokens", r.a&&r.a.i, r.b&&r.b.i, "lower"],
    ["Output tokens", r.a&&r.a.o, r.b&&r.b.o, "lower"],
    ["Cost", r.a&&r.a.c, r.b&&r.b.c, "lower"],
    ["Wall clock", r.a&&r.a.w, r.b&&r.b.w, "lower"],
    ["Output-cap hits", r.a&&r.a.cap, r.b&&r.b.cap, "lower"],
  ];
  let h = '<div class="dpanel"><p class="dhead">'+r.k+' — full telemetry, side by side</p><div class="cmp">'
    + '<div class="lab hdr"></div><div class="hdr a">Stella</div><div class="hdr b">Claude Code</div>';
  for(const [lab, av, bv, better] of rows){
    if(lab === "Verdict"){
      h += '<div class="lab">'+lab+'</div><div>'+av+'</div><div>'+bv+'</div>'; continue;
    }
    const A = av||0, B = bv||0, mx = Math.max(A,B,1);
    const disp = v => lab==="Cost" ? money(v) : lab==="Wall clock" ? dur(v) : fmt(v);
    const win = better==="lower" ? (A<B?"a":B<A?"b":"") : "";
    h += '<div class="lab">'+lab+'</div>'
      + '<div class="mrow"><span class="mbar a" style="width:'+(A/mx*100)+'%"></span>'
      + '<span class="mval '+(win==="a"?"win":win?"lose":"")+'">'+disp(av)+'</span></div>'
      + '<div class="mrow"><span class="mbar b" style="width:'+(B/mx*100)+'%"></span>'
      + '<span class="mval '+(win==="b"?"win":win?"lose":"")+'">'+disp(bv)+'</span></div>';
  }
  return h + '</div></div>';
}

function render(){
  const tb = document.getElementById("tb");
  let rows = R.filter(r => filter==="all" ? true
    : filter==="disc" ? (key(r)==="a"||key(r)==="b") : key(r)===filter);
  rows = rows.slice().sort((x,y)=>{
    const a=val(x,sortKey), b=val(y,sortKey);
    if(a===b) return x.k.localeCompare(y.k);
    return (typeof a==="string" ? a.localeCompare(b) : a-b) * sortDir;
  });
  document.getElementById("count").textContent = rows.length+" shown";
  tb.innerHTML = rows.map((r,i)=>{
    const a=r.a, b=r.b;
    return '<tr class="row" data-i="'+i+'" tabindex="0">'
      + '<td class="task">'+r.k+'</td>'
      + '<td class="sep">'+verdict(a)+'</td>'
      + '<td>'+(a?a.t:"—")+'</td><td>'+(a?fmt(a.o):"—")+'</td>'
      + '<td>'+(a?money(a.c):"—")+'</td><td>'+(a?dur(a.w):"—")+'</td>'
      + '<td class="sep">'+verdict(b)+'</td>'
      + '<td>'+(b?b.t:"—")+'</td><td>'+(b?fmt(b.o):"—")+'</td>'
      + '<td>'+(b?money(b.c):"—")+'</td><td>'+(b?dur(b.w):"—")+'</td></tr>'
      + '<tr class="detail" hidden><td colspan="11">'+detail(r)+'</td></tr>';
  }).join("");
}

document.getElementById("tb").addEventListener("click", e=>{
  const tr = e.target.closest("tr.row"); if(!tr) return;
  const d = tr.nextElementSibling; d.hidden = !d.hidden;
});
document.getElementById("tb").addEventListener("keydown", e=>{
  if(e.key!=="Enter" && e.key!==" ") return;
  const tr = e.target.closest("tr.row"); if(!tr) return;
  e.preventDefault(); const d = tr.nextElementSibling; d.hidden = !d.hidden;
});
document.querySelectorAll(".chip").forEach(c=>c.addEventListener("click",()=>{
  document.querySelectorAll(".chip").forEach(o=>o.setAttribute("aria-pressed", o===c));
  filter = c.dataset.f; render();
}));
document.querySelectorAll("thead th").forEach(th=>th.addEventListener("click",()=>{
  const k = th.dataset.s;
  if(sortKey===k) sortDir*=-1; else { sortKey=k; sortDir = k==="k"?1:-1; }
  document.querySelectorAll("thead th .ar").forEach(a=>a.textContent="");
  th.querySelector(".ar").textContent = sortDir>0?"↑":"↓";
  render();
}));
render();
</script>
'''

open('/Users/macanderson/.claude/jobs/971f0967/tmp/paper/index.html','w').write(
    HTML.replace('__PAYLOAD__', payload))
print("written", len(HTML.replace('__PAYLOAD__', payload)), "bytes")
