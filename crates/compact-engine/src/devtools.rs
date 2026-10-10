//! The developer tools drawer (⌘⌥I) and the performance measurement behind
//! it and the agent's `performance` tool.
//!
//! Like the review overlay, the drawer is page-level HTML in Compact's own
//! content world and a closed shadow root: the page cannot see or drive it.
//! Its console and network entries are not gathered here — they are the
//! capture buffers ([`crate::capture`]) the agent already reads, pushed in by
//! the shell — so you and the agent are always looking at the same thing.
//! Performance is measured on demand from the page's own performance
//! timeline by [`PERF_SCRIPT`].
//!
//! It posts on the review handler ([`crate::review::REVIEW_HANDLER`]), which
//! lives in the same world: `{"type":"devtools-ready"}` once per document,
//! `{"type":"devtools-close"}`, `{"type":"devtools-dock", "dock"}` when you
//! move it, and `{"type":"comment", …}` for "Send to
//! agent", which becomes an ordinary review comment with the entry quoted.

/// Measure the page. A `callAsyncJavaScript` body that resolves to a JSON
/// string. Metrics this engine does not report come back as `"unsupported"`
/// rather than as a misleading zero.
pub const PERF_SCRIPT: &str = r#"
  const r = v => (v == null || Number.isNaN(v)) ? null : Math.round(v);
  const supported = t => (PerformanceObserver.supportedEntryTypes || []).includes(t);
  const observe = type => new Promise(done => {
    if (!supported(type)) return done(null);
    const list = [];
    const o = new PerformanceObserver(l => list.push(...l.getEntries()));
    o.observe({ type, buffered: true });
    setTimeout(() => { o.disconnect(); done(list); }, 80);
  });
  const nav = performance.getEntriesByType('navigation')[0];
  const paint = Object.fromEntries(performance.getEntriesByType('paint').map(p => [p.name, p.startTime]));
  const [lcp, shifts, longs] = await Promise.all([observe('largest-contentful-paint'), observe('layout-shift'), observe('longtask')]);
  const res = performance.getEntriesByType('resource');
  return JSON.stringify({
    url: location.href,
    ttfb_ms: nav ? r(nav.responseStart - nav.startTime) : null,
    dom_content_loaded_ms: nav ? r(nav.domContentLoadedEventEnd) : null,
    load_ms: nav && nav.loadEventEnd ? r(nav.loadEventEnd) : null,
    first_contentful_paint_ms: r(paint['first-contentful-paint']),
    largest_contentful_paint_ms: lcp ? (lcp.length ? r(lcp[lcp.length - 1].startTime) : null) : 'unsupported',
    cumulative_layout_shift: shifts ? Math.round(shifts.filter(s => !s.hadRecentInput).reduce((a, s) => a + s.value, 0) * 1000) / 1000 : 'unsupported',
    long_tasks: longs ? { count: longs.length, total_ms: r(longs.reduce((a, t) => a + t.duration, 0)) } : 'unsupported',
    resources: {
      count: res.length,
      transfer_kb: Math.round(res.reduce((a, x) => a + (x.transferSize || 0), 0) / 1024),
      slowest: res.slice().sort((a, b) => b.duration - a.duration).slice(0, 5)
        .map(x => ({ url: x.name, ms: r(x.duration), kind: x.initiatorType })),
    },
    dom_nodes: document.getElementsByTagName('*').length,
  });
"#;

/// Hide Compact's own page UI and wait until a frame without it is drawn,
/// so a screenshot shows only the page. A `callAsyncJavaScript` body.
pub const HIDE_UI_SCRIPT: &str = r#"
  for (const t of ['compact-review', 'compact-devtools'])
    for (const h of document.getElementsByTagName(t)) { h.__compactWas = h.style.display; h.style.display = 'none'; }
  await new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done)));
  return 'hidden';
"#;

/// Put back what [`HIDE_UI_SCRIPT`] hid.
pub const SHOW_UI_SCRIPT: &str = r#"(() => {
  for (const t of ['compact-review', 'compact-devtools'])
    for (const h of document.getElementsByTagName(t)) { if ('__compactWas' in h) { h.style.display = h.__compactWas; delete h.__compactWas; } }
  return 'shown';
})()"#;

/// Open or close the drawer docked at `dock` (`bottom`, `right` or `left`),
/// flashing entry `seq` if given.
pub fn open_script(on: bool, seq: Option<u64>, agent: &str, dock: &str) -> String {
    let seq = seq.map_or("null".to_string(), |s| s.to_string());
    format!(
        "(() => {{ if (!window.__compactDevtools) return 'absent'; window.__compactDevtools.open({on}, {seq}, {agent}, {dock}); return 'ok'; }})()",
        agent = crate::inspect::js_string(agent),
        dock = crate::inspect::js_string(dock),
    )
}

/// Hand the drawer entries it has not seen. `entries` is a JSON array.
pub fn add_script(entries_json: &str, reset: bool) -> String {
    format!(
        "(() => {{ if (!window.__compactDevtools) return 'absent'; window.__compactDevtools.add({entries_json}, {reset}); return 'ok'; }})()"
    )
}

/// The drawer, dormant until opened. `__PERF__` is replaced with
/// [`PERF_SCRIPT`] by [`devtools_script`].
const DEVTOOLS_SCRIPT: &str = r##"(() => {
  if (window.__compactDevtools || window.top !== window) return;
  const post = m => { try { window.webkit.messageHandlers.compactReview.postMessage(JSON.stringify(m)); } catch (_) {} };
  const measure = async () => { __PERF__ };
  const CSS = `
    :host { all: initial; }
    * { box-sizing: border-box; font-family: -apple-system, system-ui, sans-serif; }
    .drawer { position: fixed; pointer-events: auto; background: #121216; color: #dcdce4; display: flex; flex-direction: column;
      font-size: 12px; }
    .drawer.bottom { left: 0; right: 0; bottom: 0; height: 38vh; min-height: 220px; border-top: 1px solid rgba(255,255,255,.12);
      box-shadow: 0 -10px 30px rgba(0,0,0,.35); }
    .drawer.right, .drawer.left { top: 0; bottom: 0; width: min(46vw, 560px); min-width: 340px; }
    .drawer.right { right: 0; border-left: 1px solid rgba(255,255,255,.12); box-shadow: -10px 0 30px rgba(0,0,0,.35); }
    .drawer.left { left: 0; border-right: 1px solid rgba(255,255,255,.12); box-shadow: 10px 0 30px rgba(0,0,0,.35); }
    .bar { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; padding: 6px 10px; border-bottom: 1px solid rgba(255,255,255,.08); flex: none; }
    .docks { display: flex; gap: 2px; margin-right: 6px; }
    .dock { width: 24px; height: 20px; padding: 0; border: 0; border-radius: 5px; background: transparent; cursor: pointer;
      display: flex; align-items: center; justify-content: center; }
    .dock:hover { background: rgba(255,255,255,.08); }
    .dock.on { background: rgba(124,108,240,.25); }
    .dock svg { display: block; }
    .side .filter { max-width: none; min-width: 120px; }
    .side .who { display: none; }
    .side .net { grid-template-columns: 30px 46px 1fr 52px 64px; }
    .side .net .m, .side .net .k { display: none; }
    .side .row .src { display: none; }
    .tab { background: transparent; border: 0; color: #9a9aa6; padding: 5px 10px; border-radius: 6px; font-size: 12px; cursor: pointer; }
    .tab.on { background: rgba(255,255,255,.08); color: #fff; }
    .badge { display: inline-block; min-width: 16px; padding: 0 5px; margin-left: 5px; border-radius: 8px; background: #e5484d; color: #fff; font-size: 10px; line-height: 16px; text-align: center; }
    .badge:empty { display: none; }
    .filter { margin-left: 10px; flex: 1; max-width: 280px; background: #0b0b0e; border: 1px solid rgba(255,255,255,.1); color: #eee;
      border-radius: 6px; padding: 5px 8px; font-size: 12px; outline: none; }
    label.only { color: #9a9aa6; margin-left: 8px; display: flex; gap: 5px; align-items: center; cursor: pointer; }
    .spacer { flex: 1; }
    .who { color: #7a7a86; margin-right: 8px; }
    .x { background: transparent; border: 0; color: #9a9aa6; font-size: 16px; cursor: pointer; padding: 0 6px; }
    .list { overflow: auto; flex: 1; font: 12px/1.45 ui-monospace, Menlo, monospace; }
    .row { display: flex; gap: 10px; padding: 4px 12px; border-bottom: 1px solid rgba(255,255,255,.04); align-items: flex-start; position: relative; }
    .row:hover { background: rgba(255,255,255,.035); }
    .row .seq { color: #55555f; flex: none; width: 34px; text-align: right; }
    .row .msg { flex: 1; white-space: pre-wrap; word-break: break-word; overflow: hidden; max-height: 4.4em; cursor: pointer; }
    .row.open .msg { max-height: none; }
    .row .src { color: #6b6b75; flex: none; max-width: 30%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .row.warn { background: rgba(245,166,35,.07); color: #f5c26b; }
    .row.error { background: rgba(229,72,77,.09); color: #ff8b8e; }
    .row.navigation { color: #7c6cf0; font-family: -apple-system, sans-serif; }
    .row.flash { animation: flash 1.6s ease-out 2; }
    @keyframes flash { 0%, 40% { background: rgba(124,108,240,.45); } }
    .net { display: grid; grid-template-columns: 34px 52px 54px 1fr 70px 64px 64px; gap: 10px; }
    .net .url { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .net.failed { color: #ff8b8e; background: rgba(229,72,77,.07); }
    /* Its own column at the end of the row: it never covers data, and
       nothing moves when it appears. */
    .send { flex: none; width: 64px; visibility: hidden; background: transparent; color: #c4bcff;
      border: 1px solid rgba(124,108,240,.55); border-radius: 5px; padding: 1px 0; font: 11px/16px -apple-system, sans-serif;
      cursor: pointer; justify-self: end; }
    .send:hover { background: #7c6cf0; border-color: #7c6cf0; color: #fff; }
    .row:hover .send, .row.composing .send { visibility: visible; }
    .perfsend { padding: 0 14px 14px; }
    .perfsend .send { visibility: visible; width: auto; padding: 3px 12px; }
    .composer { padding: 8px 12px 10px 56px; background: #17171c; border-bottom: 1px solid rgba(255,255,255,.08); display: flex; gap: 8px; }
    .composer textarea { flex: 1; min-height: 46px; background: #0b0b0e; color: #eee; border: 1px solid #7c6cf0; border-radius: 6px;
      padding: 6px 8px; font: 12px/1.4 -apple-system, sans-serif; outline: none; resize: vertical; }
    .composer button { align-self: flex-end; background: #7c6cf0; color: #fff; border: 0; border-radius: 6px; padding: 5px 12px; font-size: 12px; cursor: pointer; }
    .composer button.ghost { background: transparent; border: 1px solid rgba(255,255,255,.14); color: #c9c9d3; }
    .sent { color: #6ee7b7; padding: 4px 12px 4px 56px; font-family: -apple-system, sans-serif; }
    .perf { padding: 14px; display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 10px; font-family: -apple-system, sans-serif; }
    .metric { background: #17171c; border: 1px solid rgba(255,255,255,.07); border-radius: 8px; padding: 10px 12px; }
    .metric .k { color: #8a8a96; font-size: 11px; } .metric .v { font-size: 20px; color: #fff; margin-top: 4px; }
    .metric .v.na { font-size: 13px; color: #6b6b75; }
    .perfbar { padding: 10px 14px 0; display: flex; gap: 8px; align-items: center; font-family: -apple-system, sans-serif; color: #8a8a96; }
    .perfbar button { background: #26262d; color: #eee; border: 1px solid rgba(255,255,255,.1); border-radius: 6px; padding: 5px 12px; cursor: pointer; }
    .slow { padding: 0 14px 14px; font: 12px/1.6 ui-monospace, Menlo, monospace; color: #b4b4be; }
    .empty { padding: 20px; color: #6b6b75; font-family: -apple-system, sans-serif; }
  `;

  // Little window pictograms: the filled band is where the drawer goes.
  const frame = band => `<svg width="16" height="12" viewBox="0 0 16 12"><rect x=".5" y=".5" width="15" height="11" rx="2" fill="none" stroke="#9a9aa6"/>${band}</svg>`;
  const ICON = {
    bottom: frame('<rect x="1" y="7" width="14" height="4" rx="1" fill="#9a9aa6"/>'),
    right: frame('<rect x="10" y="1" width="5" height="10" rx="1" fill="#9a9aa6"/>'),
    left: frame('<rect x="1" y="1" width="5" height="10" rx="1" fill="#9a9aa6"/>'),
  };
  let dock = 'bottom';
  let host = null, root = null, on = false, view = 'console', entries = [], filter = '', only = false, agent = 'the agent';
  let perf = null, drawnView = null, composerFor = null, sent = new Set(), opened = new Set(), flashSeq = null, listEl, countC, countN;
  const el = (t, c, text) => { const e = document.createElement(t); if (c) e.className = c; if (text != null) e.textContent = text; return e; };
  const rank = { debug: 0, log: 1, info: 2, warn: 3, error: 4, navigation: 5 };
  const failed = e => e.status === 0 || e.status >= 400;

  const mount = () => {
    host = document.createElement('compact-devtools');
    host.style.cssText = 'all: initial; position: fixed; inset: 0; z-index: 2147483646; pointer-events: none;';
    root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `<style>${CSS}</style><div class="drawer"><div class="bar">
      <button class="tab" data-v="console">Console<span class="badge c"></span></button>
      <button class="tab" data-v="network">Network<span class="badge n"></span></button>
      <button class="tab" data-v="performance">Performance</button>
      <input class="filter" placeholder="Filter">
      <label class="only"><input type="checkbox"> <span>Problems only</span></label>
      <span class="spacer"></span><span class="who"></span>
      <span class="docks">${['bottom', 'right', 'left'].map(d => `<button class="dock" data-d="${d}" title="Dock ${d}">${ICON[d]}</button>`).join('')}</span>
      <button class="x" title="Close (Esc)">×</button>
      </div><div class="list"></div></div>`;
    listEl = root.querySelector('.list');
    countC = root.querySelector('.badge.c'); countN = root.querySelector('.badge.n');
    root.querySelectorAll('.tab').forEach(b => b.addEventListener('click', () => { view = b.dataset.v; composerFor = null; draw(); if (view === 'performance' && !perf) runPerf(); }));
    root.querySelector('.filter').addEventListener('input', e => { filter = e.target.value.toLowerCase(); draw(); });
    root.querySelector('.only input').addEventListener('change', e => { only = e.target.checked; draw(); });
    root.querySelector('.x').addEventListener('click', () => post({ type: 'devtools-close' }));
    root.querySelectorAll('.dock').forEach(b => b.addEventListener('click', () => {
      if (dock === b.dataset.d) return;
      dock = b.dataset.d; place(); draw();
      post({ type: 'devtools-dock', dock });
    }));
    // Keys typed into the drawer are the drawer's, not the page's: they reach
    // the drawer's own fields first, then stop here before bubbling on to
    // the page's listeners.
    for (const t of ['keydown', 'keypress', 'keyup', 'input']) host.addEventListener(t, e => e.stopPropagation());
    document.documentElement.appendChild(host);
  };

  const summary = e => e.type === 'console'
    ? `Console ${e.level} (#${e.seq}): ${e.text}${e.source ? `\nat ${e.source}` : ''}${e.stack ? `\n${e.stack}` : ''}`
    : `${e.kind} ${e.method} ${e.url} → ${e.status || 'no response'}${e.ms != null ? ` in ${e.ms} ms` : ''}${e.error ? ` (${e.error})` : ''} (#${e.seq})`;

  const composer = (subject, text) => {
    const c = el('div', 'composer');
    const ta = el('textarea');
    ta.placeholder = `What should ${agent} do about this?`;
    const send = el('button', null, `Send to ${agent}`), cancel = el('button', 'ghost', 'Cancel');
    const go = () => {
      const body = ta.value.trim() || `Please look into this.`;
      post({ type: 'comment', kind: 'change', body: `${body}\n\n${text}`, url: location.href, rect: [0, 0, 0, 0],
        viewport: [innerWidth, innerHeight], scroll: [scrollX, scrollY], selector: '', text: '', role: '', data: '', styles: '',
        framework: '', component: '', source: '' });
      sent.add(subject); composerFor = null; draw();
    };
    send.addEventListener('click', go);
    cancel.addEventListener('click', () => { composerFor = null; draw(); });
    ta.addEventListener('keydown', e => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); go(); } });
    c.append(ta, cancel, send);
    setTimeout(() => ta.focus(), 0);
    return c;
  };

  const sendButton = (subject, text) => {
    const b = el('button', 'send', 'Send ↗');
    b.title = `Send to ${agent}`;
    b.addEventListener('click', ev => { ev.stopPropagation(); composerFor = subject; draw(); });
    return b;
  };

  const after = (row, subject, text) => {
    if (composerFor === subject) row.classList.add('composing');
    listEl.append(row);
    if (composerFor === subject) listEl.append(composer(subject, text));
    else if (sent.has(subject)) listEl.append(el('div', 'sent', `Sent to ${agent}`));
  };

  const drawConsole = () => {
    const rows = entries.filter(e => e.type === 'console' || e.level === 'navigation')
      .filter(e => !only || rank[e.level] >= 3)
      .filter(e => !filter || (e.text + ' ' + (e.source || '')).toLowerCase().includes(filter));
    if (!rows.length) return listEl.append(el('div', 'empty', only ? 'No warnings or errors.' : 'Nothing logged yet.'));
    for (const e of rows) {
      if (e.level === 'navigation') { listEl.append(el('div', 'row navigation', `— loaded ${e.text}`)); continue; }
      const r = el('div', `row ${e.level}${opened.has(e.seq) ? ' open' : ''}${e.seq === flashSeq ? ' flash' : ''}`);
      r.dataset.seq = e.seq;
      const msg = el('div', 'msg', e.text + (opened.has(e.seq) && e.stack ? '\n' + e.stack : ''));
      msg.addEventListener('click', () => { opened.has(e.seq) ? opened.delete(e.seq) : opened.add(e.seq); draw(); });
      r.append(el('span', 'seq', e.seq), msg, el('span', 'src', (e.source || '').replace(location.origin, '')));
      const s = 'c' + e.seq;
      r.append(sendButton(s, summary(e)));
      after(r, s, summary(e));
    }
  };

  const drawNetwork = () => {
    const rows = entries.filter(e => e.type === 'net').filter(e => !only || failed(e))
      .filter(e => !filter || (e.url + ' ' + e.method + ' ' + e.status).toLowerCase().includes(filter));
    const head = el('div', 'row net'); head.style.color = '#6b6b75';
    for (const [h, c] of [['#'], ['Status'], ['Method', 'm'], ['URL'], ['Kind', 'k'], ['Time'], ['']]) head.append(el('span', c || null, h));
    listEl.append(head);
    if (!rows.length) return listEl.append(el('div', 'empty', only ? 'No failed requests.' : 'No requests recorded yet.'));
    for (const e of rows) {
      const r = el('div', `row net${failed(e) ? ' failed' : ''}${e.seq === flashSeq ? ' flash' : ''}`);
      r.dataset.seq = e.seq;
      const u = el('span', 'url', e.url.replace(location.origin, '')); u.title = e.url + (e.error ? '\n' + e.error : '');
      r.append(el('span', 'seq', e.seq), el('span', null, e.status || (e.error ? 'failed' : '—')), el('span', 'm', e.method), u,
        el('span', 'k', e.kind), el('span', null, e.ms != null ? e.ms + ' ms' : ''));
      u.title = `${e.method} ${e.url}${e.error ? '\n' + e.error : ''}`;
      const s = 'n' + e.seq;
      r.append(sendButton(s, summary(e)));
      after(r, s, summary(e));
    }
  };

  const fmt = v => v === 'unsupported' ? ['not reported by this engine', true] : v == null ? ['—', true] : [String(v), false];
  const metric = (k, v, unit = '') => {
    const m = el('div', 'metric'); const [t, na] = fmt(v);
    m.append(el('div', 'k', k), el('div', 'v' + (na ? ' na' : ''), na ? t : t + unit)); return m;
  };
  const drawPerf = () => {
    const bar = el('div', 'perfbar');
    const again = el('button', null, perf ? 'Measure again' : 'Measuring…');
    again.addEventListener('click', runPerf);
    bar.append(again, el('span', null, perf ? `for ${perf.url.replace(location.origin, '') || '/'}` : ''));
    listEl.append(bar);
    if (!perf) return;
    const g = el('div', 'perf');
    const lt = perf.long_tasks;
    g.append(metric('Time to first byte', perf.ttfb_ms, ' ms'), metric('DOM content loaded', perf.dom_content_loaded_ms, ' ms'),
      metric('Load', perf.load_ms, ' ms'), metric('First contentful paint', perf.first_contentful_paint_ms, ' ms'),
      metric('Largest contentful paint', perf.largest_contentful_paint_ms, typeof perf.largest_contentful_paint_ms === 'number' ? ' ms' : ''),
      metric('Layout shift (CLS)', perf.cumulative_layout_shift),
      metric('Long tasks', typeof lt === 'object' && lt ? `${lt.count} · ${lt.total_ms} ms` : lt),
      metric('Resources', `${perf.resources.count} · ${perf.resources.transfer_kb} KB`), metric('DOM nodes', perf.dom_nodes));
    listEl.append(g);
    if (perf.resources.slowest.length) {
      const s = el('div', 'slow');
      s.append(el('div', null, 'Slowest resources'));
      for (const x of perf.resources.slowest) s.append(el('div', null, `${String(x.ms).padStart(6)} ms  ${x.kind.padEnd(10)} ${x.url.replace(location.origin, '')}`));
      listEl.append(s);
    }
    const text = `Performance of ${perf.url}:\n` + JSON.stringify(perf, null, 2);
    const r = el('div', 'perfsend');
    const b = sendButton('perf', text); b.textContent = `Send these numbers to ${agent}`;
    r.append(b);
    listEl.append(r);
    if (composerFor === 'perf') listEl.append(composer('perf', text));
    else if (sent.has('perf')) listEl.append(el('div', 'sent', `Sent to ${agent}`));
  };
  const runPerf = async () => { perf = null; if (view === 'performance') draw(); try { perf = JSON.parse(await measure()); } catch (_) { perf = null; } draw(); };

  const place = () => {
    const d = root.querySelector('.drawer');
    d.className = 'drawer ' + dock + (dock === 'bottom' ? '' : ' side');
    root.querySelectorAll('.dock').forEach(b => b.classList.toggle('on', b.dataset.d === dock));
  };

  const draw = () => {
    if (!host) return;
    // A different tab starts at its own beginning, not where the last one
    // was scrolled to.
    const switched = drawnView !== view;
    drawnView = view;
    const top = switched ? 0 : listEl.scrollTop, atEnd = !switched && listEl.scrollHeight - listEl.clientHeight - top < 30;
    root.querySelectorAll('.tab').forEach(b => b.classList.toggle('on', b.dataset.v === view));
    countC.textContent = entries.filter(e => e.type === 'console' && e.level === 'error').length || '';
    countN.textContent = entries.filter(e => e.type === 'net' && failed(e)).length || '';
    root.querySelector('.who').textContent = `shared with ${agent}`;
    root.querySelector('.filter').style.visibility = view === 'performance' ? 'hidden' : '';
    root.querySelector('.only').style.visibility = view === 'performance' ? 'hidden' : '';
    listEl.textContent = '';
    if (view === 'console') drawConsole(); else if (view === 'network') drawNetwork(); else drawPerf();
    const f = flashSeq != null && listEl.querySelector(`[data-seq="${flashSeq}"]`);
    if (f) { f.scrollIntoView({ block: 'center' }); flashSeq = null; }
    else if (atEnd && !composerFor && view !== 'performance') listEl.scrollTop = listEl.scrollHeight; else listEl.scrollTop = top;
  };

  addEventListener('keydown', e => {
    if (!on || (e.target && e.target.tagName === 'COMPACT-REVIEW')) return;
    const typing = root && root.activeElement && /^(INPUT|TEXTAREA)$/.test(root.activeElement.tagName);
    if (e.key === 'Escape') {
      e.preventDefault(); e.stopImmediatePropagation();
      if (composerFor) { composerFor = null; draw(); } else if (typing) root.activeElement.blur(); else post({ type: 'devtools-close' });
      return;
    }
  }, true);

  window.__compactDevtools = {
    open(next, seq, who, where) {
      if (who) agent = who;
      if (where === 'bottom' || where === 'right' || where === 'left') dock = where;
      if (next && !host) mount();
      if (host) place();
      on = !!next;
      if (!host) return;
      host.style.display = on ? '' : 'none';
      if (seq != null) {
        flashSeq = seq;
        const hit = entries.find(e => e.seq === seq);
        if (hit) view = hit.type === 'net' ? 'network' : 'console';
      }
      if (on) draw();
    },
    add(list, reset) {
      if (reset) entries = [];
      if (Array.isArray(list)) entries.push(...list);
      if (entries.length > 1500) entries = entries.slice(-1500);
      if (on && view !== 'performance') draw();
    },
  };
  post({ type: 'devtools-ready' });
})();"##;

/// The drawer script, ready to inject.
pub fn devtools_script() -> String {
    DEVTOOLS_SCRIPT.replace("__PERF__", PERF_SCRIPT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drawer_carries_the_same_measurement_the_agent_gets() {
        let s = devtools_script();
        assert!(!s.contains("__PERF__"));
        assert!(s.contains("largest_contentful_paint_ms"));
    }

    #[test]
    fn scripts_quote_what_they_are_given() {
        assert!(open_script(true, Some(7), "Claude \"Code\"", "right").contains(r#"open(true, 7, "Claude \"Code\"", "right")"#));
        assert!(open_script(false, None, "x", "bottom").contains("open(false, null,"));
        assert!(add_script("[]", true).contains("add([], true)"));
    }

    #[test]
    fn hiding_covers_both_of_compacts_page_ui() {
        for s in [HIDE_UI_SCRIPT, SHOW_UI_SCRIPT] {
            assert!(s.contains("compact-review") && s.contains("compact-devtools"));
        }
    }
}
