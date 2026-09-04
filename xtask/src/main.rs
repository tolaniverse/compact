//! Compact's build system.
//!
//! `cargo xtask <task>` — build logic as an ordinary Rust binary in the
//! workspace, reached through an alias in `.cargo/config.toml`.
//!
//! This is the closest Rust idiom to Zig's `build.zig`: the build is a program
//! you can read, debug and step through, not a declarative file that has to
//! grow an escape hatch the first time it meets something real. `build.rs` is
//! not an alternative — it runs per-crate at compile time and cannot bundle an
//! app, drive a guarded measurement run, or sequence tasks.
//!
//! Bundling is not cosmetic. A bare Mach-O binary has no bundle identifier, and
//! `WKWebView` needs one to reach its network and storage services; the app
//! launches and then fails to load anything. Every runnable task here therefore
//! goes through `bundle`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const BUNDLE_ID: &str = "app.compact.Compact";
const DISPLAY_NAME: &str = "Compact";
/// macOS 11 is the floor for the WebKit APIs hibernation depends on:
/// `interactionState` arrived in macOS 12, but the rest of the app builds and
/// runs below it, so the deployment target is set where the UI stops working.
const MIN_MACOS: &str = "11.0";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let (task, rest) = match args.split_first() {
        Some((t, r)) => (t.as_str(), r),
        None => ("help", &[][..]),
    };

    let result = match task {
        "bundle" => bundle_task(rest).map(|_| ()),
        "run" => run_task(rest),
        "spike" => spike_task(rest),
        "dist" => dist_task(),
        "test" => test_task(),
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        other => Err(format!("unknown task `{other}`\n\nrun `cargo xtask help`")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\x1b[31merror:\x1b[0m {e}");
            ExitCode::FAILURE
        }
    }
}

fn help() {
    println!(
        r#"Compact build tasks

  cargo xtask bundle [--release] [--bin NAME]
        Build and assemble an .app bundle into out/.

  cargo xtask run [--release]
        Bundle Compact and launch it, with output attached.

  cargo xtask spike [-- SPIKE ARGS]
        Bundle and run the guarded memory harness.
        e.g. cargo xtask spike -- --tabs 12 --live 4

  cargo xtask dist
        Release bundle, ready to ship.

  cargo xtask test
        Workspace tests.
"#
    );
}

// ---- helpers ---------------------------------------------------------------

fn workspace_root() -> PathBuf {
    // `xtask/` is always exactly one level below the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live in the workspace")
        .to_path_buf()
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{what}: {e}"))?;
    if !status.success() {
        return Err(format!("{what} failed with {status}"));
    }
    Ok(())
}

struct BuildOpts {
    release: bool,
    bin: String,
}

fn parse_build_opts(args: &[String], default_bin: &str) -> BuildOpts {
    let mut o = BuildOpts { release: false, bin: default_bin.to_string() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--release" => o.release = true,
            "--bin" => {
                if let Some(b) = it.next() {
                    o.bin = b.clone();
                }
            }
            _ => {}
        }
    }
    o
}

fn info_plist(bin: &str, name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>{name}</string>
  <key>CFBundleDisplayName</key>       <string>{name}</string>
  <key>CFBundleExecutable</key>        <string>{bin}</string>
  <key>CFBundleIdentifier</key>        <string>{BUNDLE_ID}</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key>           <string>{version}</string>
  <key>LSMinimumSystemVersion</key>    <string>{MIN_MACOS}</string>
  <key>NSHighResolutionCapable</key>   <true/>
  <!-- The window draws its own chrome, including the traffic lights. -->
  <key>NSPrincipalClass</key>          <string>NSApplication</string>
  <!-- Reaching arbitrary sites is the entire point of a browser. -->
  <key>NSAppTransportSecurity</key>
  <dict><key>NSAllowsArbitraryLoads</key><true/></dict>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// Build `bin` and assemble it into `out/<Name>.app`. Returns the bundle path.
fn bundle_task(args: &[String]) -> Result<PathBuf, String> {
    let opts = parse_build_opts(args, "compact");
    let root = workspace_root();

    let mut c = Command::new(cargo());
    c.current_dir(&root).args(["build", "--package", &opts.bin]);
    if opts.release {
        c.arg("--release");
    }
    run(&mut c, &format!("building {}", opts.bin))?;

    let profile = if opts.release { "release" } else { "debug" };
    let exe = root.join("target").join(profile).join(&opts.bin);
    if !exe.exists() {
        return Err(format!("expected a binary at {}", exe.display()));
    }

    let app_name = if opts.bin == "compact" { DISPLAY_NAME.to_string() } else { opts.bin.clone() };
    let app = root.join("out").join(format!("{app_name}.app"));
    let macos_dir = app.join("Contents").join("MacOS");

    // Rebuild the bundle from scratch: a stale Info.plist that no longer
    // matches the binary produces failures that look like code bugs.
    if app.exists() {
        fs::remove_dir_all(&app).map_err(|e| format!("clearing {}: {e}", app.display()))?;
    }
    fs::create_dir_all(&macos_dir).map_err(|e| format!("creating bundle: {e}"))?;
    fs::create_dir_all(app.join("Contents").join("Resources"))
        .map_err(|e| format!("creating Resources: {e}"))?;

    fs::copy(&exe, macos_dir.join(&opts.bin)).map_err(|e| format!("copying binary: {e}"))?;
    fs::write(app.join("Contents").join("Info.plist"), info_plist(&opts.bin, &app_name))
        .map_err(|e| format!("writing Info.plist: {e}"))?;
    fs::write(app.join("Contents").join("PkgInfo"), "APPL????")
        .map_err(|e| format!("writing PkgInfo: {e}"))?;

    // Ad-hoc signature. Unsigned bundles are killed on launch on Apple silicon,
    // so this is required even for local development.
    run(
        Command::new("codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&app),
        "codesigning",
    )?;

    println!("bundled {}", app.display());
    Ok(app)
}

fn run_task(args: &[String]) -> Result<(), String> {
    let app = bundle_task(args)?;
    let exe = app.join("Contents").join("MacOS").join("compact");
    println!("launching {}", exe.display());
    // Exec the binary inside the bundle rather than `open`ing it, so stdout and
    // stderr stay attached to this terminal.
    run(&mut Command::new(&exe), "running Compact")
}

fn spike_task(args: &[String]) -> Result<(), String> {
    // Everything after `--` belongs to the harness.
    let (ours, theirs) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &[][..]),
    };
    let mut build_args: Vec<String> = ours.to_vec();
    build_args.push("--bin".into());
    build_args.push("memspike".into());

    let app = bundle_task(&build_args)?;
    let exe = app.join("Contents").join("MacOS").join("memspike");
    println!();
    run(Command::new(&exe).args(theirs), "running the spike")
}

fn dist_task() -> Result<(), String> {
    bundle_task(&["--release".to_string()]).map(|_| ())
}

fn test_task() -> Result<(), String> {
    run(
        Command::new(cargo()).current_dir(workspace_root()).args(["test", "--workspace"]),
        "tests",
    )
}
