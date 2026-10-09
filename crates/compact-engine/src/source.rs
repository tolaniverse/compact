//! Which component, in which file, made an element.
//!
//! Frameworks keep that in development builds, but as JavaScript properties
//! on DOM nodes — React's fiber, Vue's component instance, Svelte's
//! `__svelte_meta` — and properties are per content world. The review
//! overlay runs in Compact's own world and cannot see them, so this small
//! hook runs in the page's world instead, in agent workspaces only.
//!
//! It never acts on its own. The overlay marks one element with
//! [`PROBE_ATTR`] and dispatches [`PROBE_EVENT`] on the document; event
//! dispatch is synchronous across worlds, so by the time `dispatchEvent`
//! returns the hook has written what it found to [`ANSWER_ATTR`] on that
//! element, and the overlay reads it and removes both marks.
//!
//! **The answer is the page's own say-so**, like everything else read from a
//! page: a page could report any file it liked. It is passed to the agent as
//! a lead to check, and cut to size before it goes anywhere.

pub const PROBE_EVENT: &str = "compact-source-probe";
pub const PROBE_ATTR: &str = "data-compact-probe";
pub const ANSWER_ATTR: &str = "data-compact-source";

pub const SOURCE_HOOK: &str = r#"(() => {
  if (window.top !== window || window.__compactSourceHook) return;
  window.__compactSourceHook = true;
  const PROBE = 'data-compact-probe', OUT = 'data-compact-source';
  // Same-origin URLs become paths; queries such as Vite's `?t=` go.
  const clean = f => {
    if (!f) return '';
    let s = String(f);
    try { const u = new URL(s, location.href); if (u.origin === location.origin) s = u.pathname; } catch (_) {}
    return s.replace(/[?#].*$/, '');
  };
  const nameOf = t => (t && (t.displayName || t.name || (t.render && (t.render.displayName || t.render.name)) || (t.type && nameOf(t.type)))) || '';
  const VENDOR = /node_modules|react-dom|jsx-dev-runtime|react\.development|\/chunk-|\.vite\/deps|esm\.sh|unpkg\.com|jsdelivr/;
  // React 19 keeps an Error where the element was created instead of a
  // source object; its first frame outside React is the JSX line.
  const fromStack = st => {
    const s = typeof st === 'string' ? st : st && st.stack;
    if (!s) return null;
    for (const line of s.split('\n')) {
      const m = line.match(/((?:https?|file):\/\/[^\s)]+|\/[^\s):]+):(\d+):(\d+)/);
      if (m && !VENDOR.test(m[1])) return { file: clean(m[1]), line: +m[2], column: +m[3] };
    }
    return null;
  };
  const fromDebugSource = d => d && d.fileName ? { file: clean(d.fileName), line: d.lineNumber, column: d.columnNumber } : null;
  const react = el => {
    const k = Object.keys(el).find(k => k.startsWith('__reactFiber$') || k.startsWith('__reactInternalInstance$'));
    if (!k) return null;
    const fiber = el[k];
    let src = fromDebugSource(fiber._debugSource) || fromStack(fiber._debugStack);
    let component = '', via = '';
    for (let f = fiber.return; f; f = f.return) {
      const t = f.type;
      if (typeof t === 'function' || (t && typeof t === 'object' && (t.render || t.type))) {
        component = nameOf(t);
        // React 19 records only a stack, and JavaScriptCore drops the
        // component's own frame from it when the JSX is returned directly
        // (a proper tail call). The next best line is where the component
        // itself is used, and the answer says that is what it is.
        if (!src) {
          src = fromDebugSource(f._debugSource) || fromStack(f._debugStack);
          if (src) via = component;
        }
        if (component) break;
      }
    }
    return { framework: 'react', component, via, ...(src || {}) };
  };
  const vue = el => {
    for (let e = el; e; e = e.parentElement) {
      const i = e.__vueParentComponent;
      if (!i) continue;
      const t = i.type || {};
      const file = clean(t.__file);
      return { framework: 'vue', component: t.name || t.__name || (file ? file.split('/').pop().replace(/\.vue$/, '') : ''), file };
    }
    return null;
  };
  const svelte = el => {
    for (let e = el; e; e = e.parentElement) {
      const m = e.__svelte_meta;
      if (!m || !m.loc) continue;
      const file = clean(m.loc.file);
      return { framework: 'svelte', component: file ? file.split('/').pop().replace(/\.svelte$/, '') : '', file, line: m.loc.line, column: m.loc.column };
    }
    return null;
  };
  const angular = el => {
    const ng = window.ng;
    if (!ng || typeof ng.getOwningComponent !== 'function') return null;
    const c = ng.getOwningComponent(el) || ng.getComponent(el);
    return c ? { framework: 'angular', component: c.constructor && c.constructor.name } : null;
  };
  // Build plugins such as react-dev-inspector leave the location in plain
  // attributes instead.
  const attributes = el => {
    for (let e = el; e && e.getAttribute; e = e.parentElement) {
      const f = e.getAttribute('data-inspector-relative-path') || e.getAttribute('data-source-file');
      if (f) return { framework: 'attributes', file: clean(f), line: +(e.getAttribute('data-inspector-line') || e.getAttribute('data-source-line')) || undefined };
    }
    return null;
  };
  document.addEventListener('compact-source-probe', () => {
    const el = document.querySelector('[' + PROBE + ']');
    if (!el) return;
    let found = null;
    for (const f of [react, vue, svelte, angular, attributes]) {
      try { found = f(el); } catch (_) { found = null; }
      if (found && (found.component || found.file)) break;
      found = null;
    }
    el.setAttribute(OUT, found ? JSON.stringify(found) : '');
  }, true);
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_and_the_overlay_agree_on_names() {
        assert!(SOURCE_HOOK.contains(&format!("'{PROBE_ATTR}'")));
        assert!(SOURCE_HOOK.contains(&format!("'{ANSWER_ATTR}'")));
        assert!(SOURCE_HOOK.contains(&format!("'{PROBE_EVENT}'")));
        assert!(crate::review::REVIEW_SCRIPT.contains(PROBE_EVENT), "the overlay asks with the same event");
    }

    #[test]
    fn the_hook_only_ever_writes_its_answer() {
        assert!(!SOURCE_HOOK.contains("postMessage"), "it talks to no one");
        assert!(!SOURCE_HOOK.contains("fetch("));
    }
}
