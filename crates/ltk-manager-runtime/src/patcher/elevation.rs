//! Decides whether a patcher session runs unelevated or through the UAC bridge.

use super::error::PatcherError;
use crate::diagnostics;

/// How a session reaches League's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationPlan {
    /// The host runs as the manager does. League is not elevated, so injection crosses no
    /// integrity boundary.
    Unelevated,
    /// The host runs at high integrity through `--elevate`, and loads its DLL from the
    /// administrator-only install directory.
    Elevated,
}

/// Reads the running system and returns the plan for a session.
///
/// Probes the system, so it can only be called for real. The rule itself is
/// [`plan_elevation`].
pub fn plan_for_system() -> Result<ElevationPlan, PatcherError> {
    let manager_elevated = diagnostics::manager_is_elevated();
    let league_admin = diagnostics::league_configured_as_admin();
    let league_elevated = diagnostics::league_is_elevated();
    tracing::info!(
        manager_elevated,
        league_admin,
        league_elevated,
        "Patcher elevation check"
    );

    plan_elevation(manager_elevated, league_admin, league_elevated).inspect_err(|refusal| {
        let facts = diagnostics::elevation::facts();
        let cause = facts.cause().map(|cause| cause.to_string());
        tracing::warn!(%refusal, ?facts, cause, "Refusing to start the patcher");
    })
}

/// Returns the plan for a session, or the error that refuses the start.
///
/// An elevated manager refuses the start: a host it spawns already runs at high integrity and
/// would load a DLL from a directory the user can write, which ADR-0074 rules out. An
/// unelevated manager elevates its host when League is elevated either by configuration or at
/// runtime, and the host then loads the DLL from the install directory.
fn plan_elevation(
    manager_elevated: bool,
    league_admin: bool,
    league_elevated: bool,
) -> Result<ElevationPlan, PatcherError> {
    if manager_elevated {
        return Err(PatcherError::ManagerElevated);
    }
    if league_admin || league_elevated {
        return Ok(ElevationPlan::Elevated);
    }

    Ok(ElevationPlan::Unelevated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[test]
    fn plan_is_unelevated_if_nothing_is_elevated() {
        assert_matches!(
            plan_elevation(false, false, false),
            Ok(ElevationPlan::Unelevated)
        );
    }

    #[test]
    fn plan_is_elevated_if_league_is_configured_as_admin() {
        assert_matches!(
            plan_elevation(false, true, false),
            Ok(ElevationPlan::Elevated)
        );
    }

    /// A client started by an elevated launcher sets no `RUNASADMIN` layer, so the runtime
    /// check is what makes it patchable.
    #[test]
    fn plan_is_elevated_if_league_runs_elevated() {
        assert_matches!(
            plan_elevation(false, false, true),
            Ok(ElevationPlan::Elevated)
        );
    }

    #[test]
    fn plan_refuses_an_elevated_manager() {
        for league_admin in [false, true] {
            for league_elevated in [false, true] {
                assert_matches!(
                    plan_elevation(true, league_admin, league_elevated),
                    Err(PatcherError::ManagerElevated)
                );
            }
        }
    }
}
