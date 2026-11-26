# Internet Control — Usage Meter (v1.0.1)

A Windows 10/11 (x64) internet usage meter written in Rust. It shows
**real, precise upload and download** — including over PDAnet phone
tethering + VPN, where many other tools show 0 or double-count — plus a
per-app list with **Block / Unblock**, like NetLimiter.

- Native UI built with [Slint](https://slint.dev) (software renderer):
  no WebView2/Edge, no GPU requirement.
- Metering with WinDivert in **sniff mode**: packets are only copied,
  never held or re-injected, so the meter can never slow down or stall
  your connection.
- Per-app blocking with the **Windows Filtering Platform** (WFP):
  dynamic filters that disappear automatically when the app closes.

Next step: per-app speed limiting.

## What's new

### v1.0.1
- **Fixed: buttons stopped responding a few seconds after start.**
  Pressing the title bar (or the Minimal view background) started a
  native window move on mouse-down; Windows' move loop swallowed the
  button-up, so the UI kept the title bar "pressed" and sent every later
  click to it. The window now moves only when you actually drag it, and
  a release is delivered to the UI afterwards. Double-click on the title
  bar works again.
- **Calm, exact numbers.** All speeds (top numbers, per-app, adapters,
  Peak) are recomputed once per second from the exact byte count of
  that second — like Task Manager — instead of a 10×/second smoothed
  value that jittered.
- **Stable graph.** The 60-second graph has one fixed point per second;
  older points no longer move up and down while the graph scrolls.
- The Apps list (sorted by rate) re-orders at most once per second.

### v1.0.0
- **New engine:** WinDivert sniff (`recv_only`) for counting. v0.9 sent
  all system traffic through one user-space thread (intercept +
  re-inject), which could add latency or stall the connection.
- **New blocking:** WFP dynamic-session filters per app (new
  connections, IPv4 + IPv6, outbound + inbound), a kernel drop handle
  for sockets that were already open, and immediate termination of the
  app's TCP connections. Everything is removed when Internet Control
  exits.
- Adapter enumeration every 2 s in capture mode (was 10×/second).
- **New UI (Slint, Fluent style):** borderless window with its own
  title bar, dark/light theme, Overview / Apps / Adapters tabs, Minimal
  view, always-on-top pin, opacity slider, 60-second graph.

Kept from v0.9: full exe path per app, System (PID 4), Unknown traffic
with Identify, public IP + country, Auto / All / selected adapters,
start with Windows, config in `%APPDATA%\internet-control\config.json`,
log file next to the exe.

## Install

1. Download `internet-control-v1.0.1-windows-x64.zip` from Releases.
2. Unzip it anywhere. **Keep these files together in one folder:**
   `internet-control.exe`, `WinDivert.dll`, `WinDivert64.sys`.
3. Run `internet-control.exe` and accept the UAC prompt — WinDivert and
   WFP need administrator rights. Without them the app falls back to
   adapter counters (inaccurate on PDAnet) and blocking is unavailable.

No installer, no internet connection needed to run it.

## How to use

- **Overview:** live download / upload, session and today totals, peak,
  the adapter the numbers come from, and the 60-second graph. The status
  line should read `Packet capture (sniff) · … · blocking ready`.
- **Apps:** per-app speed (Live rate) or total (Total). Search, sort by
  clicking a column header. **Block** cuts an app off at once;
  **Unblock** gives it back. Select a row to copy its exe path or show it
  in Explorer. The Unknown panel lists unattributed traffic; **Identify**
  looks up its owner.
- **Adapters:** **Auto** (follows the adapter carrying traffic), **All**
  (sum of everything — VPN over PDAnet is then counted twice), or tick
  specific adapters. The Diagnostics box shows capture totals per
  interface; copy it into bug reports.
- **Minimal:** a small window with the rates, graph, IP and opacity.
  Drag it anywhere; double-click to go back to the full view.
- Pin keeps the window on top; the theme, opacity, size and pin are saved.

## Verification test

1. Restart the app (session starts at 0).
2. Download a file of known size over PDAnet, e.g. **100 MB**.
3. Session should read **~100 MB** (1–3% more for TCP/IP headers).
   - **~200 MB** → double counting (VPN + PDAnet both selected): choose
     Auto or only PDAnet on the Adapters tab.
   - **0** → wrong adapter or WinDivert did not start: check the status
     line and the log.
4. Block one app (e.g. a browser): it should lose internet immediately
   while everything else keeps working. Unblock restores it.

## If something looks wrong

Close the app and send the last lines of **`internet-control.log`**
(next to the exe). Useful lines: `capture status: …`,
`blocker: WFP …`, `Block <exe>: …`.

## Known limitations

- The drop rule for sockets that were already open matches on local
  port; in rare cases another app's connection that uses the same local
  port can be dropped while a block is active.
- Per-app speed limiting is not implemented yet.

## Build from source

The `windivert` crate needs the WinDivert 2.2.2 files at build time.
Download
https://github.com/basil00/WinDivert/releases/download/v2.2.2/WinDivert-2.2.2-A.zip
and set `WINDIVERT_PATH` to its `x64` folder:

```powershell
$env:WINDIVERT_PATH = "$PWD\WinDivert-2.2.2-A\x64"
cargo build --release --target x86_64-pc-windows-msvc
```

The exe is in `target\x86_64-pc-windows-msvc\release\`. Copy
`WinDivert.dll` and `WinDivert64.sys` next to it. The administrator
manifest is embedded by `build.rs`.

Cross-compiling from Linux works with
[cargo-xwin](https://github.com/rust-cross/cargo-xwin):
`cargo xwin build --release --target x86_64-pc-windows-msvc`.

`cargo test` runs the platform-independent unit tests on any OS (on
Linux install `libfontconfig1-dev` and `libxkbcommon-dev` first).

## License

MIT for Internet Control itself. `WinDivert.dll` and `WinDivert64.sys`
come from the official WinDivert 2.2.2 release by Basil00
(https://reqrypt.org/windivert.html) and are under their own license
(LGPLv3 / GPLv2) — see `WinDivert-LICENSE.txt` in the release zip.
