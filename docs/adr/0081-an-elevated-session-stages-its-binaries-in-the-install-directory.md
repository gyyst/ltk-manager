# ADR-0081: An elevated session stages its binaries in the install directory

- **Status:** Accepted
- **Crates:** `ltk-manager-runtime`, `ltk-manager-core`, `src-tauri`
- **Related:** Changes the elevation rule of [ADR-0074](0074-every-session-runs-a-delivered-patcher-dll.md).
  Restores the `--elevate` host mode that ADR-0074 kept unused, under the directory rule of
  [ADR-0055](0055-ltk-manager-installs-per-machine.md).

## Context and problem statement

ADR-0074 refuses every session while League runs at high integrity, and points the reader at the
`RUNASADMIN` compatibility layer. That covers a client the reader configured to run as
administrator, but not a client that an elevated launcher starts. WeGame declares
`requireAdministrator` in its manifest, so the League client inherits a high-integrity token and
carries no compatibility layer. The patcher then starts unelevated and the DLL never attaches.

The refusal also caught unrelated tools. The compatibility scan matched any executable whose name
starts with `League` or `Riot`, so a `RUNASADMIN` layer on `LeagueAkari.exe` refused every start.

## Decision

**An elevated session runs its host through the UAC bridge.** When the manager is unelevated and
League is either configured as administrator or runs at high integrity, the session spawns the
host with `--elevate`, which raises one UAC prompt and runs the host at high integrity.

**An elevated host loads its binaries from the install directory.** The delivered DLL sits under
`<app data>/patcher/<digest>`, which the user can write, so an elevated session copies the verified
host and DLL into `<install dir>/patcher/<digest>`. The copy runs in a helper of the manager
executable, `--stage-elevated-patcher`, which holds the token the UAC prompt grants. The manager
then verifies the copy while still unelevated: the DLL hashes to the digest, and the directory
refuses a write.

**The manager refuses to elevate itself.** A start still fails when the manager runs elevated,
because a host it spawns inherits high integrity without the staging step and would load the app
data DLL.

**The compatibility scan matches exact executable names.** The list is the League client, the
game, the Riot Client and its helpers. A third-party tool that starts with `League` no longer
refuses a start.

## Consequences

**A WeGame client can be patched.** The reader accepts one UAC prompt per patcher start. A
dismissed prompt ends the session with an error that names the prompt.

**The staging directory is a new location under the install directory.** A per-machine install
puts it under `Program Files`, where ADR-0055's ACL already covers it. An install elsewhere fails
the staging step and reports the directory rule, so the reader installs under `Program Files`.

**Every start verifies the copy.** A digest mismatch or a writable staging directory refuses the
session rather than running the app data DLL, so a failed verification never falls back to an
unprotected binary.

**The elevated host still needs the delivered DLL.** A build without an attestation reads the
DLLs that an official install staged under the same data directory, so a fork without the
delivery attestation can patch only where an official install has staged a DLL.
