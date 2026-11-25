# ADR-0082: The Sona LTF launcher keeps the debugging port

- **Status:** Accepted (2026-11-25)
- **Crates:** `ltk-manager`, in `webview_args`, `shell_relaunch` and `setup`
- **Related:** Changes the rule of [ADR-0080](0080-the-manager-filters-its-browser-arguments-and-closes-a-debuggable-webview.md)
  for one launcher.

## Context and problem statement

This repository is the LTK Manager fork that Sona ships. The LTF bridge drives the manager's
page over the Chrome DevTools Protocol. `ltk_launcher.exe`, in Sona's repository at
`lib/ltk-bridge/native/ltf/LeagueToolFlop-inj.cpp`, sets
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` with `--remote-debugging-port` and
`--remote-allow-origins`, injects `LeagueToolFlop.dll`, and reads the port back from
`LTF_CDP_PORT`.

ADR-0080 closes that path. `webview_args::take_inherited` drops every switch in
`DEBUG_SWITCHES`, `shell_relaunch::relaunch_if_needed` restarts a debugging start under
explorer, and `verify_browser_process` closes a manager whose browser process carries such a
switch. A manager started by the LTF launcher then either restarts without the port or exits,
and the bridge reports `CDP unavailable or timed out`. Sona keeps the port for this start only.

## Decision

**The Sona LTF launcher marks its start.** `ltk_launcher.exe` adds `SONA_LTF_BRIDGE=1` to the
`EnvVars` it writes before it starts the manager, beside the two WebView2 variables.

**The manager reads and removes the marker.** `webview_args::take_inherited` reads
`SONA_LTF_BRIDGE` before any thread runs, removes it from the environment, and stores it as
`Inherited.sona_ltf`. The marker does not reach WebView2 or a child process of the manager.

**A marked start keeps the debugging switches.** `Inherited::new` moves a switch in
`DEBUG_SWITCHES` from `dropped` to `kept` when `sona_ltf` is set, so `setup` passes it to WebView2
through the API. Other switches keep the ADR-0080 behavior.

**A marked start does not restart for debugging.** `shell_relaunch::relaunch_if_needed` treats
`is_debugging()` as false when `sona_ltf` is set. The parent check is unchanged.

**A marked start is not closed for a debugging switch.** `verify_browser_process` still refuses a
`BrowserExecutableFolder` policy, which replaces the runtime, and still reads the browser
process. A debugging switch in that command line does not close the manager. An unmarked start
keeps the ADR-0080 check.

**An unmarked start is unchanged.** Without `SONA_LTF_BRIDGE`, the filter, the restart and the
close behave as ADR-0080 states.

### Considered options

1. **A marker set only by the Sona launcher.** Chosen.
2. **Keep the port for every start.** It removes the ADR-0080 defense for installers, browsers
   and other launchers.
3. **Replace the protocol before the first release.** It is the durable fix, but it needs a
   local authenticated API in the manager and a new client in the bridge, and the bridge has no
   such API to call today.
4. **Start the manager with the marker from the bridge process.** The bridge is not the process
   that starts the manager, and the manager must not depend on the bridge.

## Consequences

**The marker is not authentication.** Any process of the same Windows user can set
`SONA_LTF_BRIDGE` before it starts the manager, and the manager then keeps a debugging port it
did not ask for. The marker records a start path, not a verified caller. The trust assumption is
that the user's own environment is trusted, which matches what a same-user process can already
do. The registry policy check and the process command line check still run, so a runtime folder
replacement closes the manager.

**The LTF bridge keeps working.** A manager started by `ltk_launcher.exe` keeps the port on its
command line, and `LeagueToolFlop.dll` reaches it. The manager's own commands decide what the
page may do, so the page is not a trust boundary either way.

**A marked start is visible in the log.** The startup line reports `sona_ltf` beside the kept and
dropped switches and the launcher port.

**The durable fix removes the exception.** When the manager offers a local authenticated API for
the operations the bridge drives, the bridge stops using CDP, and this ADR and its marker can be
deleted. ADR-0080 remains the rule for every other launcher.
