//! The patcher's domain errors.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::delivery::DeliveryFailure;
use super::injector::InjectorError;
use super::session::SessionError;
use super::staging::StagingError;
use ltk_manager_base::error::{AppError, DomainError, ErrorKind};

/// The message of [`PatcherError::StagingRefused`], which names the rule and the way out.
pub const STAGING_REFUSED_MESSAGE: &str = "LTK Manager cannot stage the patcher in its install \
    directory. Install LTK Manager under Program Files and start the patcher again.";

/// Which stage of a start failed, for [`PatcherError::InjectionFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InjectionStage {
    /// The host process never came up - it could not be spawned, configured, or
    /// told to start. Antivirus or a missing binary are the usual causes, so
    /// diagnostics are the useful response.
    #[strum(to_string = "host startup")]
    Host,
    /// The host ran, but the game was never patched.
    #[strum(to_string = "DLL injection")]
    Injection,
}

/// Domain errors specific to the patcher.
///
/// Sent over IPC as the `context` payload of an `AppError` with code `PATCHER`.
/// Frontend code can switch on `kind` to handle each variant.
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[cfg_attr(feature = "ts", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PatcherError {
    /// Something that mutates the mod library was rejected because a session is
    /// live and pointed at the very files it would rewrite. The caller has to
    /// stop the patcher first - unlike [`Self::AlreadyRunning`], this is not a
    /// stale-status problem.
    #[error("Stop the patcher before modifying mods")]
    Busy,

    /// A start was requested while a session is already live. Benign: it usually
    /// just means the caller's view of the status is stale.
    #[error("The patcher is already running")]
    AlreadyRunning,

    /// A stop was requested while nothing was running.
    #[error("The patcher is not running")]
    NotRunning,

    #[error("The patcher is not yet available on this platform")]
    UnsupportedPlatform,

    /// League is set to run as administrator. Older managers refuse a start in this situation.
    /// The current manager stages an elevated host instead.
    #[error(
        "League runs as administrator. Start the patcher again and accept the User Account \
         Control prompt"
    )]
    LeagueElevated,

    /// LTK Manager runs as administrator, and a session runs unelevated.
    #[error(
        "LTK Manager is running as administrator. Reopen it normally, or turn on User Account \
         Control if it is off, to start the patcher"
    )]
    ManagerElevated,

    /// The install directory cannot hold a copy of the host and DLL that an elevated session may
    /// load. `message` names the directory rule and the way out.
    #[error("{message}")]
    StagingRefused { message: String },

    /// The overlay could not be built, so no host ran. `message` is the build's own words.
    #[error("{message}")]
    BuildFailed { message: String },

    /// No patcher DLL could be selected for the session. `reason` is the delivery failure's
    /// kind, which the frontend turns into copy.
    #[error("no patcher DLL is available: {reason}")]
    DllUnavailable { reason: DeliveryFailure },

    /// One injection session failed. `stage` separates a host that never came up
    /// from a game that was never patched, since they call for different advice.
    #[error("{message}")]
    InjectionFailed {
        stage: InjectionStage,
        message: String,
    },
}

impl From<PatcherError> for AppError {
    fn from(error: PatcherError) -> Self {
        Self::Domain(DomainError::new(ErrorKind::Patcher, error))
    }
}

impl From<SessionError> for PatcherError {
    fn from(error: SessionError) -> Self {
        // Classified by the innermost error, not by how deep it nested: a
        // `HostError` reaching us through the injector is still a host failure.
        let stage = match &error {
            SessionError::Host(_) | SessionError::Injector(InjectorError::Host(_)) => {
                InjectionStage::Host
            }
            SessionError::Injector(InjectorError::Failed(_)) => InjectionStage::Injection,
        };
        Self::InjectionFailed {
            stage,
            message: error.to_string(),
        }
    }
}

impl From<StagingError> for PatcherError {
    fn from(error: StagingError) -> Self {
        match error {
            // Both refusals mean the same thing to the reader: the install directory cannot
            // protect a high-integrity host, so the manager has to move under Program Files.
            StagingError::InstallDirUnprotected { .. } | StagingError::DirectoryWritable { .. } => {
                Self::StagingRefused {
                    message: STAGING_REFUSED_MESSAGE.to_string(),
                }
            }
            other => Self::InjectionFailed {
                stage: InjectionStage::Host,
                message: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patcher::host::HostError;

    #[test]
    fn a_host_that_never_came_up_is_a_host_stage_failure() {
        let error = PatcherError::from(SessionError::Host(HostError::StdoutClosed));
        assert!(matches!(
            error,
            PatcherError::InjectionFailed {
                stage: InjectionStage::Host,
                ..
            }
        ));
    }

    /// A `HostError` reaching us through the injector is still a host failure -
    /// the stage is decided by the innermost error, not by the nesting.
    #[test]
    fn a_host_error_through_the_injector_is_still_a_host_stage_failure() {
        let error = PatcherError::from(SessionError::Injector(InjectorError::Host(
            HostError::StdinClosed,
        )));
        assert!(matches!(
            error,
            PatcherError::InjectionFailed {
                stage: InjectionStage::Host,
                ..
            }
        ));
    }

    #[test]
    fn a_dll_that_never_attached_is_an_injection_stage_failure() {
        let error = PatcherError::from(SessionError::Injector(InjectorError::Failed(
            "DLL never attached after 60s".to_string(),
        )));
        assert!(matches!(
            error,
            PatcherError::InjectionFailed {
                stage: InjectionStage::Injection,
                message
            } if message.contains("DLL never attached")
        ));
    }

    /// The `kind` tag is the frontend's switch; unit variants must carry it too.
    #[test]
    fn variants_serialize_with_a_kind_tag() {
        let json = serde_json::to_value(PatcherError::AlreadyRunning).unwrap();
        assert_eq!(json["kind"], "ALREADY_RUNNING");

        let json = serde_json::to_value(PatcherError::InjectionFailed {
            stage: InjectionStage::Host,
            message: "host died".to_string(),
        })
        .unwrap();
        assert_eq!(json["kind"], "INJECTION_FAILED");
        assert_eq!(json["stage"], "HOST");
        assert_eq!(json["message"], "host died");
    }

    /// A staging refusal is a different failure from an elevated League, so the bridge and the
    /// frontend can point the reader at the install directory instead of the compatibility layer.
    #[test]
    fn an_unprotected_install_directory_is_a_staging_refusal() {
        let error = PatcherError::from(StagingError::InstallDirUnprotected {
            path: "C:\\Tools\\LTK Manager".to_string(),
        });

        assert!(matches!(error, PatcherError::StagingRefused { .. }));

        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["kind"], "STAGING_REFUSED");
        assert!(
            json["message"].as_str().unwrap().contains("Program Files"),
            "the message must name the install rule"
        );
    }

    #[test]
    fn a_writable_staging_directory_is_a_staging_refusal() {
        let error = PatcherError::from(StagingError::DirectoryWritable {
            path: "C:\\Users\\me\\AppData\\Local\\Programs\\LTK Manager".to_string(),
        });

        assert!(matches!(error, PatcherError::StagingRefused { .. }));
    }

    #[test]
    fn a_build_failure_carries_its_message() {
        let error = PatcherError::BuildFailed {
            message: "the game path is not configured".to_string(),
        };

        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["kind"], "BUILD_FAILED");
        assert_eq!(json["message"], "the game path is not configured");
    }

    /// A dismissed prompt reaches the bridge as a host-stage failure, carrying the text it maps
    /// to its own advice.
    #[test]
    fn a_dismissed_uac_prompt_is_a_host_stage_failure() {
        let error = PatcherError::from(StagingError::Helper {
            message: "the user dismissed the User Account Control prompt".to_string(),
        });

        assert!(matches!(
            error,
            PatcherError::InjectionFailed {
                stage: InjectionStage::Host,
                message,
            } if message.contains("User Account Control")
        ));
    }
}
