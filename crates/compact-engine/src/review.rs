//! The review overlay: the page script behind ⌘⇧R.
//!
//! Injected into agent-workspace pages in Compact's own content world
//! ([`crate::inspect::WORLD`]) and dormant until the shell switches it on, so
//! the page can neither see its state nor post on its handler — which is
//! registered in that world alone. Its elements live in a closed shadow root,
//! so the page's styles cannot reach them either.
//!
//! What the page *can* still notice is that something is there: a host
//! element appears, and events pass through window listeners it registered
//! first. Nothing it does there can make a comment, read one, or change where
//! one is pinned.
//!
//! Messages to the shell, as JSON strings on [`REVIEW_HANDLER`]:
//!
//! - `{"type":"ready"}` once per document, so the shell can switch a
//!   reloaded page back on.
//! - `{"type":"comment", kind, body, url, selector, text, role, data, rect,
//!   viewport, scroll, styles}` when you send one. The overlay hides itself
//!   first, so the crop the shell takes shows the page and not the overlay.
//! - `{"type":"exit"}` on Escape with nothing open.
//!
//! The shell drives it with [`set_script`].

/// The message-handler name the overlay posts to, in Compact's world only.
pub const REVIEW_HANDLER: &str = "compactReview";

/// The script to switch the overlay on or off. `pins` is a JSON array of
/// `{id, n, state, kind, body, selector, text, role, rect, viewport, scroll}`.
pub fn set_script(on: bool, pins_json: &str) -> String {
    format!(
        "(() => {{ if (!window.__compactReview) return 'absent'; window.__compactReview.set({on}, {pins_json}); return 'ok'; }})()"
    )
}

pub const REVIEW_SCRIPT: &str = r#"(() => {
  if (window.__compactReview || window.top !== window) return;
  const post = m => { try { window.webkit.messageHandlers.compactReview.postMessage(JSON.stringify(m)); } catch (_) {} };
  const CSS = `
    :host { all: initial; }
    * { box-sizing: border-box; font-family: -apple-system, system-ui, sans-serif; }
    .layer { position: fixed; inset: 0; cursor: crosshair; pointer-events: auto; background: transparent; }
    .box { position: fixed; pointer-events: none; border: 2px solid #7c6cf0; background: rgba(124,108,240,.10);
      border-radius: 4px; display: none; transition: all .06s ease-out; }
    .tag { position: absolute; left: -2px; top: -24px; white-space: nowrap; font: 600 11px/1 ui-monospace, Menlo, monospace;
      color: #fff; background: #7c6cf0; padding: 5px 7px; border-radius: 4px; }
    .tag.below { top: auto; bottom: -24px; }
    .region { position: fixed; pointer-events: none; border: 2px dashed #34c38f; background: rgba(52,195,143,.10);
      border-radius: 4px; display: none; }
    .pin { position: fixed; width: 22px; height: 22px; margin: -11px 0 0 -11px; border-radius: 11px; pointer-events: auto;
      display: flex; align-items: center; justify-content: center; font: 700 11px/1 -apple-system, sans-serif;
      color: #fff; background: #7c6cf0; box-shadow: 0 2px 8px rgba(0,0,0,.35); cursor: pointer; }
    .pin.add { background: #34c38f; } .pin.question { background: #3b82f6; }
    .pin.addressed { outline: 2px solid #fff; animation: pulse 1.2s ease-in-out infinite; }
    .pin.lost { background: #6b6b75; }
    .pin.focus { transform: scale(1.25); }
    @keyframes pulse { 50% { box-shadow: 0 0 0 7px rgba(124,108,240,.35); } }
    .card { position: fixed; width: 320px; pointer-events: auto; background: #17171c; color: #ececf1;
      border: 1px solid rgba(255,255,255,.12); border-radius: 10px; box-shadow: 0 12px 40px rgba(0,0,0,.45); padding: 12px; }
    .card .head { display: flex; gap: 6px; align-items: center; margin-bottom: 8px; font-size: 11px; color: #9a9aa6; }
    .card .kind { border: 1px solid rgba(255,255,255,.14); background: transparent; color: #c9c9d3; border-radius: 6px;
      padding: 3px 8px; font-size: 11px; cursor: pointer; }
    .card .kind.on { background: #7c6cf0; border-color: #7c6cf0; color: #fff; }
    .card textarea { width: 100%; min-height: 72px; resize: vertical; background: #0e0e12; color: #ececf1;
      border: 1px solid rgba(255,255,255,.12); border-radius: 7px; padding: 8px; font-size: 13px; line-height: 1.4; outline: none; }
    .card textarea:focus { border-color: #7c6cf0; }
    .card .foot { display: flex; justify-content: space-between; align-items: center; margin-top: 8px; font-size: 11px; color: #7a7a86; }
    .card button.send { background: #7c6cf0; color: #fff; border: 0; border-radius: 6px; padding: 5px 12px; font-size: 12px; cursor: pointer; }
    .card .body { font-size: 13px; line-height: 1.45; white-space: pre-wrap; }
    .card .meta { font-size: 11px; color: #7a7a86; margin-top: 6px; }
  `;

  let on = false, host = null, root = null, layer, box, tag, region, pinsEl;
  let hovered = null, narrower = [], composer = null, card = null, press = null, pins = [], focusPin = -1, laid = 0;

  const ROLE = { A: 'link', BUTTON: 'button', SELECT: 'combobox', TEXTAREA: 'textbox', IMG: 'img', NAV: 'navigation',
    MAIN: 'main', HEADER: 'banner', FOOTER: 'contentinfo', ASIDE: 'complementary', FORM: 'form', UL: 'list', OL: 'list',
    LI: 'listitem', TABLE: 'table', H1: 'heading', H2: 'heading', H3: 'heading', H4: 'heading', H5: 'heading', H6: 'heading',
    INPUT: 'textbox', LABEL: 'label', SECTION: 'region', DIALOG: 'dialog' };
  const roleOf = el => el.getAttribute('role') || ROLE[el.tagName] || el.tagName.toLowerCase();
  const textOf = el => ((el.innerText || el.value || el.getAttribute('alt') || el.getAttribute('aria-label') || '') + '')
    .replace(/\s+/g, ' ').trim().slice(0, 120);
  const unique = s => { try { return document.querySelectorAll(s).length === 1; } catch (_) { return false; } };
  const selectorOf = el => {
    if (el.id && unique('#' + CSS_.escape(el.id))) return '#' + CSS_.escape(el.id);
    const tid = el.getAttribute('data-testid');
    if (tid) { const s = `[data-testid="${CSS_.escape(tid)}"]`; if (unique(s)) return s; }
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && e !== document.documentElement && parts.length < 7; e = e.parentElement) {
      if (e.id && unique('#' + CSS_.escape(e.id))) { parts.unshift('#' + CSS_.escape(e.id)); break; }
      let p = e.tagName.toLowerCase();
      const cls = [...e.classList].filter(c => /^[a-zA-Z][\w-]{1,30}$/.test(c)).slice(0, 2);
      if (cls.length) p += '.' + cls.map(c => CSS_.escape(c)).join('.');
      const par = e.parentElement;
      if (par) {
        const same = [...par.children].filter(c => c.tagName === e.tagName);
        if (same.length > 1) p += `:nth-of-type(${same.indexOf(e) + 1})`;
      }
      parts.unshift(p);
      if (unique(parts.join(' > '))) break;
    }
    return parts.join(' > ');
  };
  const CSS_ = window.CSS && CSS.escape ? CSS : { escape: s => String(s).replace(/[^\w-]/g, c => '\\' + c) };
  const dataOf = el => [...el.attributes].filter(a => a.name.startsWith('data-'))
    .slice(0, 8).map(a => a.name + '=' + a.value.slice(0, 60)).join(' ');
  const stylesOf = el => {
    const s = getComputedStyle(el);
    return ['display', 'position', 'width', 'height', 'font-size', 'font-weight', 'color', 'background-color',
      'padding', 'margin', 'overflow', 'white-space', 'text-overflow']
      .map(k => `${k}: ${s.getPropertyValue(k)}`).join('; ');
  };
  const rectOf = r => [r.x, r.y, r.width, r.height].map(v => Math.round(v * 10) / 10);

  const pick = (x, y) => {
    layer.style.pointerEvents = 'none';
    const el = document.elementFromPoint(x, y);
    layer.style.pointerEvents = '';
    return el && el !== host && el !== document.documentElement ? el : null;
  };

  const drawBox = () => {
    if (!hovered || composer) { box.style.display = 'none'; return; }
    const r = hovered.getBoundingClientRect();
    Object.assign(box.style, { display: 'block', left: r.x - 2 + 'px', top: r.y - 2 + 'px', width: r.width + 4 + 'px', height: r.height + 4 + 'px' });
    const name = textOf(hovered);
    tag.textContent = roleOf(hovered) + (name ? ' · ' + (name.length > 32 ? name.slice(0, 31) + '…' : name) : '')
      + (narrower.length ? '   ⌥↓ narrower' : '') + (hovered.parentElement && hovered.parentElement !== document.body ? '   ⌥↑ wider' : '');
    tag.classList.toggle('below', r.y < 28);
  };

  const place = (el, r) => {
    const w = 320, h = 170;
    let x = Math.min(Math.max(8, r.x), innerWidth - w - 8);
    let y = r.y + r.height + 10;
    if (y + h > innerHeight) y = Math.max(8, r.y - h - 10);
    Object.assign(el.style, { left: x + 'px', top: y + 'px' });
  };

  const closeCard = () => { if (card) { card.remove(); card = null; } composer = null; region.style.display = 'none'; drawBox(); };

  const openComposer = target => {
    closeCard();
    composer = target;
    const r = target.el ? target.el.getBoundingClientRect() : target.rect;
    card = document.createElement('div');
    card.className = 'card';
    const kinds = target.el ? ['change', 'question'] : ['add'];
    card.innerHTML = `<div class="head">${kinds.map(k => `<button class="kind" data-k="${k}">${k[0].toUpperCase() + k.slice(1)}</button>`).join('')}
      <span style="margin-left:auto"></span></div><textarea></textarea>
      <div class="foot"><span>⌘↩ send · esc cancel</span><button class="send">Send</button></div>`;
    const ta = card.querySelector('textarea');
    ta.placeholder = target.el ? 'What should change?' : 'What belongs here?';
    card.querySelector('.head span').textContent = target.el ? roleOf(target.el) : 'new area';
    const setKind = k => { composer.kind = k; card.querySelectorAll('.kind').forEach(b => b.classList.toggle('on', b.dataset.k === k)); };
    card.querySelectorAll('.kind').forEach(b => b.addEventListener('click', e => { e.stopPropagation(); setKind(b.dataset.k); ta.focus(); }));
    card.querySelector('.send').addEventListener('click', e => { e.stopPropagation(); send(); });
    setKind(kinds[0]);
    root.appendChild(card);
    place(card, r);
    box.style.display = 'none';
    setTimeout(() => ta.focus(), 0);
  };

  const send = () => {
    if (!composer || !card) return;
    const body = card.querySelector('textarea').value.trim();
    if (!body) return;
    const t = composer;
    const r = t.el ? t.el.getBoundingClientRect() : t.rect;
    const m = { type: 'comment', kind: t.kind, body, url: location.href, rect: rectOf(r), viewport: [innerWidth, innerHeight],
      scroll: [scrollX, scrollY], selector: '', text: '', role: '', data: '', styles: '' };
    if (t.el) Object.assign(m, { selector: selectorOf(t.el), text: textOf(t.el), role: roleOf(t.el), data: dataOf(t.el), styles: stylesOf(t.el) });
    closeCard();
    hovered = null;
    // Out of the crop: the shell photographs the page as soon as this
    // arrives, so wait until a frame without us has actually been drawn.
    // It shows us again once the photograph is taken.
    host.style.display = 'none';
    requestAnimationFrame(() => requestAnimationFrame(() => post(m)));
  };

  const showCard = (p, at) => {
    closeCard();
    card = document.createElement('div');
    card.className = 'card';
    const b = document.createElement('div'); b.className = 'body'; b.textContent = p.body;
    const meta = document.createElement('div'); meta.className = 'meta';
    meta.textContent = `${p.id} · ${p.kind} · ${p.state}${p.lost ? ' · element not found' : ''}`;
    card.append(b, meta);
    root.appendChild(card);
    place(card, at);
  };

  // Where a pin's element is now: selector, then text and role, then where
  // it was. A miss is flagged, never silently moved onto something else.
  const locate = p => {
    if (p.selector) {
      try {
        const el = document.querySelector(p.selector);
        if (el && (!p.text || textOf(el) === p.text || !textOf(el))) return { el };
      } catch (_) {}
      if (p.text) {
        const all = document.querySelectorAll('a,button,input,select,textarea,img,h1,h2,h3,h4,h5,h6,li,p,span,div,label,section');
        for (const el of all) if (textOf(el) === p.text && roleOf(el) === p.role) return { el };
      }
    }
    const [x, y, w, h] = p.rect, [sx, sy] = p.scroll || [0, 0];
    return { rect: { x: x + sx - scrollX, y: y + sy - scrollY, width: w, height: h }, lost: !!p.selector };
  };

  const layout = () => {
    if (!on) return;
    pinsEl.textContent = '';
    pins.forEach((p, i) => {
      const at = locate(p);
      p.lost = !!at.lost;
      const r = at.el ? at.el.getBoundingClientRect() : at.rect;
      if (r.y + r.height < 0 || r.y > innerHeight) return;
      const d = document.createElement('div');
      d.className = `pin ${p.kind} ${p.state}${p.lost ? ' lost' : ''}${i === focusPin ? ' focus' : ''}`;
      d.textContent = p.n;
      d.title = p.body;
      Object.assign(d.style, { left: Math.min(r.x + r.width, innerWidth - 14) + 'px', top: Math.max(r.y, 14) + 'px' });
      d.addEventListener('click', e => { e.stopPropagation(); focusPin = i; showCard(p, r); layout(); });
      pinsEl.appendChild(d);
    });
  };
  const relayout = () => { if (laid) return; laid = requestAnimationFrame(() => { laid = 0; layout(); drawBox(); }); };

  const jump = dir => {
    if (!pins.length) return;
    focusPin = (focusPin + dir + pins.length) % pins.length;
    const at = locate(pins[focusPin]);
    if (at.el) at.el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    else window.scrollTo({ top: at.rect.y + scrollY - innerHeight / 3, behavior: 'smooth' });
    setTimeout(() => { layout(); const a = locate(pins[focusPin]); showCard(pins[focusPin], a.el ? a.el.getBoundingClientRect() : a.rect); }, 350);
  };

  const mount = () => {
    host = document.createElement('compact-review');
    host.style.cssText = 'all: initial; position: fixed; inset: 0; z-index: 2147483647; pointer-events: none;';
    root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `<style>${CSS}</style><div class="layer"></div><div class="region"></div><div class="box"><span class="tag"></span></div><div class="pins"></div>`;
    [layer, region, box, pinsEl] = ['.layer', '.region', '.box', '.pins'].map(s => root.querySelector(s));
    tag = root.querySelector('.tag');
    layer.addEventListener('mousemove', e => {
      if (press) {
        const dx = e.clientX - press.x, dy = e.clientY - press.y;
        if (!press.drag && Math.hypot(dx, dy) > 6) { press.drag = true; closeCard(); }
        if (press.drag) {
          const r = { x: Math.min(e.clientX, press.x), y: Math.min(e.clientY, press.y), width: Math.abs(dx), height: Math.abs(dy) };
          press.rect = r;
          Object.assign(region.style, { display: 'block', left: r.x + 'px', top: r.y + 'px', width: r.width + 'px', height: r.height + 'px' });
          box.style.display = 'none';
          return;
        }
      }
      if (composer) return;
      const el = pick(e.clientX, e.clientY);
      if (el !== hovered) { hovered = el; narrower = []; drawBox(); }
    });
    layer.addEventListener('mousedown', e => { e.preventDefault(); press = { x: e.clientX, y: e.clientY, drag: false }; });
    layer.addEventListener('mouseup', e => {
      const p = press; press = null;
      if (!p) return;
      if (p.drag && p.rect && p.rect.width > 8 && p.rect.height > 8) return openComposer({ rect: p.rect, kind: 'add' });
      region.style.display = 'none';
      if (card && !composer) { closeCard(); return; }
      const el = pick(e.clientX, e.clientY);
      if (el) { hovered = el; openComposer({ el, kind: 'change' }); }
    });
    for (const t of ['click', 'dblclick', 'contextmenu', 'auxclick']) layer.addEventListener(t, e => { e.preventDefault(); e.stopPropagation(); });
    document.documentElement.appendChild(host);
  };

  addEventListener('keydown', e => {
    if (!on) return;
    const stop = () => { e.preventDefault(); e.stopImmediatePropagation(); };
    if (composer) {
      if (e.key === 'Escape') { stop(); closeCard(); }
      else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { stop(); send(); }
      else e.stopImmediatePropagation();
      return;
    }
    if (e.key === 'Escape') { stop(); if (card) closeCard(); else post({ type: 'exit' }); }
    else if (e.altKey && e.key === 'ArrowUp' && hovered && hovered.parentElement && hovered.parentElement !== document.body) {
      stop(); narrower.push(hovered); hovered = hovered.parentElement; drawBox();
    } else if (e.altKey && e.key === 'ArrowDown' && narrower.length) { stop(); hovered = narrower.pop(); drawBox(); }
    else if (e.key === 'Enter' && hovered) { stop(); openComposer({ el: hovered, kind: 'change' }); }
    else if (e.key === ']') { stop(); jump(1); }
    else if (e.key === '[') { stop(); jump(-1); }
  }, true);
  addEventListener('scroll', relayout, { capture: true, passive: true });
  addEventListener('resize', relayout, { passive: true });
  // Pages that re-render without scrolling still move things around.
  setInterval(() => { if (on && !composer) layout(); }, 700);

  window.__compactReview = {
    set(next, list) {
      pins = Array.isArray(list) ? list : [];
      if (next && !host) mount();
      on = !!next;
      if (!host) return;
      host.style.display = on ? '' : 'none';
      if (!on) { closeCard(); hovered = null; focusPin = -1; }
      layout(); drawBox();
    },
  };
  post({ type: 'ready' });
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_script_reports_whether_the_overlay_was_there() {
        let s = set_script(true, "[]");
        assert!(s.contains("set(true, [])"));
        assert!(s.contains("'absent'"));
    }

    #[test]
    fn the_overlay_posts_only_on_its_own_handler() {
        assert!(REVIEW_SCRIPT.contains(&format!("messageHandlers.{REVIEW_HANDLER}.postMessage")));
        assert!(!REVIEW_SCRIPT.contains("messageHandlers.compact."), "never the internal pages' handler");
    }
}
