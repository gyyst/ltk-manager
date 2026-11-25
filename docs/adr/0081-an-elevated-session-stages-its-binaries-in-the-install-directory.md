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

**The UAC prompt follows the overlay build.** The session prepares the delivered build first,
builds the overlay, and only then runs the staging helper. A build that fails raises no prompt, and
a dismissed prompt leaves no half-started session. A rebuild reuses the staged host, so no later
build raises a second prompt.

**A staging refusal is its own error.** The manager reports `STAGING_REFUSED` when the install
directory is not under `Program Files` or its copy is writable, so the reader is told to move the
install instead of turning off League's administrator setting.

**The patching phase follows the host's first answer.** The session claims `patching` only after
the host answers the config and scan commands it was sent. An elevated host raises its UAC prompt
after it is spawned, so claiming the phase earlier would expose `patching` while the prompt is
still open. A dismissed prompt would then look like a session that started and stopped. A host
that dies before it answers now ends the start in the building phase with an error that names the
prompt.

**The manager refuses to elevate itself.** A start still fails when the manager runs elevated,
because a host it spawns inherits high integrity without the staging step and would load the app
data DLL.

**The compatibility scan matches exact executable names.** The list is the League client, the
game, the Riot Client and its helpers. A third-party tool that starts with `League` no longer
refuses a start.

## Consequences

**A WeGame client can be patched.** The reader accepts one UAC prompt per patcher start for the
staging helper, plus one for the first host spawn of a manager run. A dismissed prompt ends the
session with an error that names the prompt.

**A failed build fails before the prompt.** A missing game path, an unavailable DLL or a failed
overlay build ends the start with its own error, and the reader sees no UAC prompt for work that
could not have run.

**A failed start stays readable after the thread ends.** The patcher status carries the last
failure, cleared by the next start, so an embedder that polls the status can tell a dismissed
prompt, a refused staging and a failed build apart without reading the log.

**The staging directory is a new location under the install directory.** A per-machine install
puts it under `Program Files`, where ADR-0055's ACL already covers it. An install elsewhere fails
the staging step and reports the directory rule, so the reader installs under `Program Files`.

**Every start verifies the copy.** A digest mismatch or a writable staging directory refuses the
session rather than running the app data DLL, so a failed verification never falls back to an
unprotected binary.

**The elevated host still needs the delivered DLL.** A build without an attestation reads the
DLLs that an official install staged under the same data directory, so a fork without the
delivery attestation can patch only where an official install has staged a DLL.
