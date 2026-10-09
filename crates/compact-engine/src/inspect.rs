//! Scripts that look at a page on an agent's behalf.
//!
//! These run in Compact's own content world ([`WORLD`]), not the page's: they
//! see the same DOM, but the page cannot see them, call them, or tamper with
//! the element references they hand out. That is what makes a `ref` from a
//! snapshot safe to act on later — a page cannot redefine it to point
//! somewhere else.
//!
//! Platform-free so the string handling can be tested without WebKit.

/// The isolated content world inspection scripts run in.
pub const WORLD: &str = "compact-agent";

/// The longest snapshot returned, in characters. A page with ten thousand
/// rows is summarised rather than sent whole.
pub const SNAPSHOT_CAP: usize = 60_000;

/// An accessibility-style outline of the page, one element per line:
///
/// ```text
/// page: Pricing — Acme
/// url: http://localhost:3000/pricing
/// viewport: 1280×800
/// - banner [ref=e1]
///   - link "Home" [ref=e2]
/// - main [ref=e3]
///   - heading "Pricing" [level=1] [ref=e4]
///   - button "Start free trial" [ref=e5]
/// ```
///
/// Refs stay valid for as long as the document lives, so an agent can name an
/// element it saw in a snapshot when it asks for a screenshot of it.
pub const SNAPSHOT_SCRIPT: &str = r#"(() => {
  const R = (window.__compactRefs ||= { next: 1, of: new WeakMap(), by: new Map() });
  const ref = el => {
    let r = R.of.get(el);
    if (!r) { r = 'e' + R.next++; R.of.set(el, r); R.by.set(r, new WeakRef(el)); }
    return r;
  };
  const CAP = __CAP__;
  const IMPLICIT = {
    A: el => el.hasAttribute('href') ? 'link' : '', BUTTON: () => 'button', SELECT: () => 'combobox',
    TEXTAREA: () => 'textbox', IMG: () => 'img', NAV: () => 'navigation', MAIN: () => 'main',
    HEADER: () => 'banner', FOOTER: () => 'contentinfo', ASIDE: () => 'complementary', FORM: () => 'form',
    UL: () => 'list', OL: () => 'list', LI: () => 'listitem', TABLE: () => 'table', TR: () => 'row',
    TH: () => 'columnheader', TD: () => 'cell', DIALOG: () => 'dialog', SECTION: el => el.hasAttribute('aria-label') ? 'region' : '',
    H1: () => 'heading', H2: () => 'heading', H3: () => 'heading', H4: () => 'heading', H5: () => 'heading', H6: () => 'heading',
    INPUT: el => ({ checkbox: 'checkbox', radio: 'radio', button: 'button', submit: 'button', reset: 'button',
      range: 'slider', search: 'searchbox', hidden: '' })[(el.type || '').toLowerCase()] ?? 'textbox',
  };
  // Roles named by their own text. Their children are not listed: the name
  // already says what they contain.
  const LEAF = new Set(['link', 'button', 'heading', 'tab', 'menuitem', 'option', 'columnheader']);
  const roleOf = el => el.getAttribute('role') || (IMPLICIT[el.tagName] ? IMPLICIT[el.tagName](el) : '');
  const clean = s => (s || '').replace(/\s+/g, ' ').trim();
  const short = s => s.length > 100 ? s.slice(0, 99) + '…' : s;
  const nameOf = (el, role) => {
    const by = el.getAttribute('aria-labelledby');
    if (by) return clean(by.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' '));
    const n = el.getAttribute('aria-label') || el.getAttribute('alt') || '';
    if (n) return clean(n);
    if (el.labels && el.labels.length) return clean(el.labels[0].textContent);
    if (LEAF.has(role))
      return clean(el.innerText || el.textContent);
    return clean(el.getAttribute('title') || el.getAttribute('placeholder') || '');
  };
  const hidden = el => {
    if (el.getAttribute('aria-hidden') === 'true' || el.hidden) return true;
    if (el.checkVisibility) return !el.checkVisibility({ visibilityProperty: true });
    const s = getComputedStyle(el);
    return s.display === 'none' || s.visibility === 'hidden';
  };
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'HEAD']);
  const out = [`page: ${document.title}`, `url: ${location.href}`, `viewport: ${innerWidth}×${innerHeight}`];
  let size = 0, cut = false;
  const emit = s => {
    if (cut) return;
    if (size + s.length > CAP) { cut = true; out.push('… truncated'); return; }
    size += s.length + 1; out.push(s);
  };
  const walk = (el, depth) => {
    if (cut || SKIP.has(el.tagName) || hidden(el)) return;
    const role = roleOf(el);
    let next = depth;
    if (role && role !== 'none' && role !== 'presentation') {
      const name = short(nameOf(el, role));
      let line = '  '.repeat(depth) + '- ' + role + (name ? ' ' + JSON.stringify(name) : '');
      if (role === 'heading') line += ` [level=${el.getAttribute('aria-level') || (/^H[1-6]$/.test(el.tagName) ? el.tagName[1] : 2)}]`;
      if (el.disabled) line += ' [disabled]';
      if (el.checked) line += ' [checked]';
      if ((role === 'textbox' || role === 'searchbox' || role === 'combobox') && el.value) line += ' value=' + JSON.stringify(short(el.value));
      emit(line + ` [ref=${ref(el)}]`);
      if (LEAF.has(role)) return;
      next = depth + 1;
    }
    // Loose text directly inside this element.
    for (const n of el.childNodes) {
      if (n.nodeType === 3) {
        const t = short(clean(n.textContent));
        if (t) emit('  '.repeat(next) + '- text ' + JSON.stringify(t));
      }
    }
    for (const c of el.children) walk(c, next);
  };
  if (document.body) walk(document.body, 0);
  return out.join('\n');
})()"#;

/// The script for [`SNAPSHOT_SCRIPT`], with its size cap filled in.
pub fn snapshot_script() -> String {
    SNAPSHOT_SCRIPT.replace("__CAP__", &SNAPSHOT_CAP.to_string())
}

/// Scroll an element into view and report its box in view coordinates, as
/// `"x,y,width,height"`. `target` is either a snapshot ref (`e12`) or a CSS
/// selector.
///
/// Never throws: an exception in our world still fires the page's `error`
/// event, which would let the page watch what the agent looks for. A miss is
/// answered with [`NO_MATCH`] instead.
pub fn locate_script(target: &str) -> String {
    format!(
        r#"(() => {{
  const t = {t};
  const R = window.__compactRefs;
  let el = null;
  try {{ el = /^e\d+$/.test(t) ? (R && R.by.get(t) && R.by.get(t).deref()) : document.querySelector(t); }} catch (_) {{}}
  if (!el || !el.isConnected) return '{miss}';
  el.scrollIntoView({{ block: 'center', inline: 'center' }});
  const r = el.getBoundingClientRect();
  return [r.x, r.y, r.width, r.height].map(v => Math.round(v * 100) / 100).join(',');
}})()"#,
        t = js_string(target),
        miss = NO_MATCH,
    )
}

/// What [`locate_script`] answers when nothing matches, an invalid selector
/// included.
pub const NO_MATCH: &str = "no-match";

/// Parse [`locate_script`]'s answer, clipped to the viewport. `None` when the
/// element has no visible area at all.
pub fn parse_rect(s: &str, viewport: (f64, f64)) -> Option<(f64, f64, f64, f64)> {
    let v: Vec<f64> = s.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let [x, y, w, h] = v[..] else { return None };
    let (x0, y0) = (x.max(0.0), y.max(0.0));
    let (x1, y1) = ((x + w).min(viewport.0), (y + h).min(viewport.1));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// A JavaScript string literal for `s`, safe to splice into a script.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            // Line and paragraph separators end a JS string literal in
            // older engines; `</script>` cannot matter here but `<` is cheap.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_cannot_break_out_of_their_string() {
        let s = js_string("a\"); alert(1); (\"\n\u{2028}\\");
        assert_eq!(s, r#""a\"); alert(1); (\"\n\u2028\\""#);
        assert!(locate_script("button[data-x=\"1\"]").contains(r#"const t = "button[data-x=\"1\"]";"#));
        assert!(!locate_script("x").contains("throw"), "a miss must not reach the page's error event");
    }

    #[test]
    fn rects_are_clipped_to_the_viewport() {
        assert_eq!(parse_rect("10,20,100,50", (1280.0, 800.0)), Some((10.0, 20.0, 100.0, 50.0)));
        assert_eq!(parse_rect("-10,780,100,50", (1280.0, 800.0)), Some((0.0, 780.0, 90.0, 20.0)));
        assert_eq!(parse_rect("10,900,100,50", (1280.0, 800.0)), None, "off screen");
        assert_eq!(parse_rect("10,20,0,50", (1280.0, 800.0)), None, "no area");
        assert_eq!(parse_rect("nonsense", (1280.0, 800.0)), None);
    }

    #[test]
    fn the_snapshot_script_carries_its_cap() {
        assert!(snapshot_script().contains(&format!("const CAP = {SNAPSHOT_CAP};")));
    }
}
