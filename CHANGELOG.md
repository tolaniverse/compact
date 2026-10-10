# Changelog

All notable changes to Compact are recorded here. Versions follow
[Semantic Versioning](https://semver.org); before 1.0, a minor version may
change behaviour.

## Unreleased

- Split view's divider can be dragged, from a quarter to three quarters of
  the width; double-click it for an even split. Where you leave it is saved.

## 0.1.0 — 2026-10-10

The first release. Apple Silicon Macs, macOS 11 or later; signed ad hoc, not
notarized (see the README's First launch).

### The browser

- Tabs live in workspaces (Personal, Work, Research), each with its own
  active tab, unread count and notification setting: All, Mentions or Muted.
- Only a budget of tabs is live at once, sized to the memory free on your Mac
  and shrunk under memory pressure. The rest hibernate as a saved session —
  687 bytes per tab, measured — and come back with their history and scroll
  position. Whether WebKit then frees the renderer behind them is still being
  measured, so no claim is made about that (FINDINGS.md, Q2).
- Chrome that stays hidden until the pointer reaches an edge or a key asks for
  it: vertical or horizontal tabs (⌘E), an address bar (⌘L), a command palette
  (⌘K), quick and full settings, and a shortcuts overlay (?).
- Every binding is remappable, and the palette and shortcuts overlay show the
  keys you actually have. The workspace switcher is ⌥Space, since macOS keeps
  ⌘Space for Spotlight.
- A notification stack for activity in other workspaces, with ⌘⇧J to jump to
  the latest.
- Bookmarks (⌘D), a new-tab page with recent pages and your workspaces, and
  error pages that say what actually went wrong.
- Split view (⌘\\): two workspaces side by side as rounded cards, each
  labelled with its workspace and page; ⌘⌥\\ or a click switches sides.
- Workspaces, tabs, bookmarks and settings are saved across restarts.
  Browsing history is not.

### Agent workspaces

- `compact-mcp`, bundled in the app, lets Claude Code, opencode or any MCP
  client connect. The first connection from a project asks you; once allowed,
  the agent gets its own workspace, marked with a robot and named after it,
  with its own cookies and storage.
- Tools to drive and read its tabs: open, navigate, reload, close and list;
  an accessibility-style snapshot with element refs; screenshots of the page
  or one element; console output, network failures and a performance
  measurement.
- Loopback addresses are always allowed, other sites need your yes once per
  session, and `file:` and similar URLs are refused. An agent can only touch
  its own workspace, waits while you have taken control of it, and can have its
  access revoked from the palette.
- Review mode (⌘⇧R): pin comments on elements of the agent's page, or drag
  out an area for something new. Comments carry the element's selector, text,
  role, styles, a cropped screenshot and, in React, Vue and Svelte dev builds,
  the component and source file. The agent replies, marks comments addressed
  (the page reloads and the pin pulses), and you resolve or reopen them.
- Developer tools (⌘⌥I): console, network and performance, docked at the
  bottom, right or left beside the page, sharing the agent's data, with Send
  on every row to hand an entry to the agent.

### Security

- Compact's own pages are the only ones that can drive it: messages from any
  website to the internal pages' handler are refused.
- The agent socket is readable by your user only. Review comments and the
  overlay run in Compact's own script world, which the page cannot reach.
