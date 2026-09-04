# Compact — memory findings

Measured 2026-09-04 on an 8 GB M1, macOS 26.5.2, via `cargo xtask spike`.
All figures are `phys_footprint` (`proc_pid_rusage`), never RSS. WebKit helpers
are attributed by pid-set difference against a pre-start baseline, because they
are launchd XPC services with ppid 1 and cannot be attributed by parentage.

---

## Q1. Does a hibernated tab stay under the 50 KB gate, driven through the real engine?

**Answer: yes — 687 B, a 75x margin. Confidence: high (measured).**

| tabs | live | sessions | min / med / max | aggregate |
|---:|---:|---:|---|---:|
| 8  | 3 | 5  | 687 B / 687 B / 687 B | 3.4 KB |
| 20 | 3 | 17 | 687 B / 687 B / 687 B | 11.4 KB |
| 30 | 3 | 27 | 687 B / 687 B / 687 B | 18.1 KB |

Consistent with the M0 spike's 640 B – 3.9 KB band. The size is constant here
because every tab loaded the same URL with no navigation history; real browsing
will sit higher in that band. Restore was verified live each run.

The live set held at 3 while 30 tabs existed, so the two-tier model works as
designed.

## Q2. Is the idle floor bounded, or does it scale with tabs ever opened?

**Answer: unresolved, and currently it scales. Confidence: high that the effect
is real; the cause is not yet established.**

After destroying *every* web view, WebContent processes survive roughly 1:1 with
tabs ever opened, and do not go away within 30 s:

| tabs opened | surviving procs | surviving WebContent |
|---:|---:|---:|
| 10 | 11 | 116 MB |
| 20 | 22 | 221 MB |
| 30 | 31 | 328 MB |

No plateau up to 30. Sampled at +0/+5/+15/+30 s — flat at every point, so this
is not WebKit's ~5 s process reuse delay.

**Hypotheses eliminated by experiment:**

- *The retained `interactionState` blob keeps the page alive.* A/B with capture
  disabled: 9 WebContent processes either way. Eliminated.
- *Autoreleased objects never drain under a hand-driven `runUntilDate`.* Added
  an autorelease pool around the pump: no change. Eliminated.

`retainCount` was 18 immediately before dropping our last reference, but that
number is unreliable on `WKWebView` and is not evidence on its own.

**The experiment that would settle it** is implemented but has not completed:
open fresh tabs *after* teardown and watch the process count. Flat means these
are WebKit's reuse cache — benign, reclaimed under pressure. Climbing means the
memory is genuinely lost.

```
cargo xtask spike -- --tabs 10 --live 3
```

It aborted mid-run on this machine when swap hit 4.08 GB. Needs a run with real
headroom.

**Consequence for the product:** the 687 B blob is real, but until Q2 is
answered, Compact should not claim that hibernating a tab returns its ~100 MB.
It currently returns the blob; the renderer behind it is still resident.

## Q3. Do the safety rails actually work?

**Answer: yes — both fired for real. Confidence: high (observed).**

- The preflight guard refused to start at 1.36 GB available against a 2 GB
  floor.
- The `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` abort tore a run down when swap
  reached 4.08 GB, rather than wedging the machine as the 2026-09-03 run did.

---

## Incidental findings

- **`WKProcessPool` is deprecated as a no-op.** Content-process sharing is now
  driven by a shared `WKWebViewConfiguration` and website data store, which is
  what the engine uses.
- **Plain `CALayer` alpha composites weakly over a `WKWebView`.** Page text read
  straight through a 96%-opaque overlay panel. Overlay scrims need
  `NSVisualEffectView`.

## Next

Answer Q2 on a machine with headroom. Until then the live-set budget is doing
the real work, and it is measured rather than assumed — the settings footer
reports actual `phys_footprint`, not a per-tab estimate.
