//! Internal pages: the new tab, the error page, and full settings.
//!
//! These three are *page content*, not chrome. The canvas models them as tab
//! kinds sitting in the viewport — they slide with the workspace transition,
//! they sit under the tear-off address bar, and they occupy a tab. So they are
//! rendered as HTML into the tab's own web view rather than drawn as layers
//! over it, which also means the load bar, the slide and the chrome need no
//! special case for them at all.
//!
//! Everything here is data. The markup lives in [`crate::html`].

use core::fmt;

/// What a tab is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PageKind {
    /// A real site.
    #[default]
    Web,
    /// The new-tab page: search, recents, pinned, workspaces.
    NewTab,
    /// A failed navigation, rendered as one of [`ErrorKind`].
    Error,
    /// The full settings page (`⌘⇧,`), as distinct from the quick drawer (`⌘,`).
    Settings,
}

impl PageKind {
    /// Whether this page is drawn by us rather than fetched.
    pub const fn is_internal(self) -> bool {
        !matches!(self, PageKind::Web)
    }
}

/// The internal URL scheme. Real enough to show in the address bar, and
/// distinctive enough that nothing on the network can claim it.
pub const NEW_TAB_URL: &str = "compact://newtab";
pub const SETTINGS_URL: &str = "compact://settings";

/// One line of the error page's "what I checked" panel.
pub struct Step {
    pub mark: &'static str,
    /// `Some(true)` passed, `Some(false)` failed, `None` not attempted.
    pub ok: Option<bool>,
    pub label: &'static str,
    pub note: &'static str,
}

/// Everything the error page renders.
pub struct ErrorSpec {
    pub code: &'static str,
    pub name: &'static str,
    /// Label on the primary button.
    ///
    /// It always retries, so it always says so. The canvas offers "Go back",
    /// "Search instead" and "Allow once" here — each of which would be a button
    /// that does something other than what it claims, since Compact has no
    /// history navigation and no request allowlist to override.
    pub action: &'static str,
    /// The large drifting glyph.
    pub glyph: &'static str,
    pub title: &'static str,
    pub body: &'static str,
    pub steps: &'static [Step],
    /// The dry one-liner at the foot of the page.
    pub sig: &'static str,
}

/// The seven failures Compact explains rather than merely reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    NotFound,
    Offline,
    Dns,
    Timeout,
    Cert,
    Crash,
    Blocked,
}

impl ErrorKind {
    /// In the canvas's declaration order, which is also the order the settings
    /// page lists the preview chips in.
    pub const ALL: [ErrorKind; 7] = [
        ErrorKind::NotFound,
        ErrorKind::Offline,
        ErrorKind::Dns,
        ErrorKind::Timeout,
        ErrorKind::Cert,
        ErrorKind::Crash,
        ErrorKind::Blocked,
    ];

    /// Stable identifier used in internal URLs and bridge messages.
    pub const fn slug(self) -> &'static str {
        match self {
            ErrorKind::NotFound => "404",
            ErrorKind::Offline => "offline",
            ErrorKind::Dns => "dns",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Cert => "cert",
            ErrorKind::Crash => "crash",
            ErrorKind::Blocked => "blocked",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.slug() == s)
    }

    /// Classify a `NSURLError` / `WKError` code.
    ///
    /// Returns `None` for codes that must not produce an error page —
    /// `NSURLErrorCancelled` above all, which is what a navigation *replaced by
    /// another navigation* reports. Treating that as a failure would flash an
    /// error page every time a page redirects or the user retypes an address
    /// mid-load.
    pub fn from_url_error(code: isize) -> Option<Self> {
        match code {
            // NSURLErrorCancelled, and WebKit's "frame load interrupted".
            -999 | 102 => None,
            -1009 => Some(ErrorKind::Offline),          // notConnectedToInternet
            -1003 | -1006 => Some(ErrorKind::Dns),      // cannotFindHost / dnsLookupFailed
            -1001 => Some(ErrorKind::Timeout),          // timedOut
            // secureConnectionFailed, serverCertificate*, clientCertificate*.
            -1206..=-1200 => Some(ErrorKind::Cert),
            // Blocked by a content rule list.
            104 => Some(ErrorKind::Blocked),
            // Everything else that actually failed: the host is there but we
            // could not get the document. "Not found" is the honest generic —
            // it is the only one of the seven that does not assert a cause.
            _ => Some(ErrorKind::NotFound),
        }
    }

    pub const fn spec(self) -> &'static ErrorSpec {
        match self {
            ErrorKind::NotFound => &NOT_FOUND,
            ErrorKind::Offline => &OFFLINE,
            ErrorKind::Dns => &DNS,
            ErrorKind::Timeout => &TIMEOUT,
            ErrorKind::Cert => &CERT,
            ErrorKind::Crash => &CRASH,
            ErrorKind::Blocked => &BLOCKED,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

static NOT_FOUND: ErrorSpec = ErrorSpec {
    code: "404",
    name: "Not found",
    action: "Reload anyway",
    glyph: "4 ◌ 4",
    title: "I looked. It's not there.",
    body: "The server answered — it just had nothing at this address. Someone probably moved the page and didn't leave a note.",
    steps: &[
        Step { mark: "✓", ok: Some(true), label: "Resolved the domain", note: "ok" },
        Step { mark: "✓", ok: Some(true), label: "Server answered", note: "HTTP 404" },
        Step { mark: "✕", ok: Some(false), label: "Found the page", note: "no such path" },
    ],
    sig: "compact · nothing was harmed in the making of this error",
};

static OFFLINE: ErrorSpec = ErrorSpec {
    code: "OFFLINE",
    name: "No internet",
    action: "Try again",
    glyph: "((  ✕  ))",
    title: "You're off the grid.",
    body: "No network at all — not slow, not blocked, absent. Everything you already loaded still works, because Compact keeps it in memory.",
    steps: &[
        Step { mark: "✕", ok: Some(false), label: "Network interface", note: "no route" },
        Step { mark: "—", ok: None, label: "DNS", note: "skipped" },
        Step { mark: "✓", ok: Some(true), label: "Cached tabs still readable", note: "live set" },
    ],
    sig: "compact · this page needed no internet to render",
};

static DNS: ErrorSpec = ErrorSpec {
    code: "DNS",
    name: "Site doesn't exist",
    action: "Try again",
    glyph: "? · ? · ?",
    title: "Nobody has heard of this name.",
    body: "I asked every resolver I know and they all shrugged. Either it's a typo, or the domain hasn't been born yet.",
    steps: &[
        Step { mark: "✓", ok: Some(true), label: "Network reachable", note: "ok" },
        Step { mark: "✕", ok: Some(false), label: "Name resolution", note: "NXDOMAIN" },
        Step { mark: "→", ok: None, label: "Try a search instead", note: "⌘L" },
    ],
    sig: "compact · check the spelling, then check it again",
};

static TIMEOUT: ErrorSpec = ErrorSpec {
    code: "TIMEOUT",
    name: "Timed out",
    action: "Retry",
    glyph: "·   ·   ·",
    title: "It's thinking. Still thinking.",
    body: "The connection opened and then nothing came back. I waited, which in browser time is a geological era.",
    steps: &[
        Step { mark: "✓", ok: Some(true), label: "Connected", note: "TCP ok" },
        Step { mark: "✓", ok: Some(true), label: "Request sent", note: "ok" },
        Step { mark: "✕", ok: Some(false), label: "Response", note: "never arrived" },
    ],
    sig: "compact · waited patiently, gave up politely",
};

static CERT: ErrorSpec = ErrorSpec {
    code: "CERT",
    name: "Insecure",
    action: "Try again",
    glyph: "[ ! ]",
    title: "This site's ID doesn't match its face.",
    body: "The certificate is for a different name than the one you typed. Usually a misconfiguration. Occasionally something worse.",
    steps: &[
        Step { mark: "✓", ok: Some(true), label: "TLS handshake", note: "started" },
        Step { mark: "✕", ok: Some(false), label: "Certificate", note: "not trusted" },
        Step { mark: "✕", ok: Some(false), label: "Chain of trust", note: "broken" },
    ],
    sig: "compact · I stopped here on purpose",
};

static CRASH: ErrorSpec = ErrorSpec {
    code: "CRASH",
    name: "Tab crashed",
    action: "Restore tab",
    glyph: "×__×",
    title: "That tab has gone to sleep forever.",
    body: "One tab fell over. Everything else kept running — that's the whole point of keeping them apart.",
    steps: &[
        Step { mark: "✕", ok: Some(false), label: "Renderer process", note: "terminated" },
        Step { mark: "✓", ok: Some(true), label: "Other tabs", note: "unaffected" },
        Step { mark: "✓", ok: Some(true), label: "Session", note: "recoverable" },
    ],
    sig: "compact · memory reclaimed, silver linings everywhere",
};

static BLOCKED: ErrorSpec = ErrorSpec {
    code: "BLOCKED",
    name: "Blocked",
    action: "Try again",
    glyph: "⊘",
    title: "I stopped this one on purpose.",
    body: "A rule matched this request before it left your machine. Nothing was sent. If that was wrong, you can override it.",
    steps: &[
        Step { mark: "✕", ok: Some(false), label: "Request", note: "blocked locally" },
        Step { mark: "✓", ok: Some(true), label: "Matched rule", note: "content filter" },
        Step { mark: "✓", ok: Some(true), label: "Data sent", note: "0 bytes" },
    ],
    sig: "compact · your business stayed your business",
};
