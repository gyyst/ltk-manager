//! Background patching thread: builds the overlay, runs the injection session,
//! rebuilds between games when the library changes, and reports everything
//! user-facing through [`PatcherEvents`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use ltk_telemetry::Telemetry;

use ltk_manager_base::advisory::Advisories;
use ltk_manager_base::config::Config;
use ltk_manager_base::error::{AppError, AppResult, message_with_sources};
use ltk_manager_base::events::{OverlayProgress, PrepareStep};
use ltk_manager_library::mods::ModLibrary;
use ltk_manager_library::overlay::{OverlayBuild, OverlayTimings, WorkshopTestProject};
use ltk_manager_runtime::diagnostics::binary_id::PatcherBinaries;
use ltk_manager_runtime::diagnostics::ids::LogIds;
use ltk_manager_runtime::diagnostics::incident::SessionFailure;
use ltk_manager_runtime::diagnostics::telemetry;
use ltk_manager_runtime::launcher::is_game_running;
use ltk_manager_runtime::patcher::delivery::StagedPatcher;

use super::pipeline::{IncidentPipeline, SessionStores};
use ltk_manager_runtime::patcher::error::PatcherError;
use ltk_manager_runtime::patcher::events::PatcherEvents;
use ltk_manager_runtime::patcher::host::{HostConfig, HostLogLevel, PatcherHost};
use ltk_manager_runtime::patcher::injector::SessionEnd;
use ltk_manager_runtime::patcher::recorder::GameRecorder;
use ltk_manager_runtime::patcher::refresh::OverlayRefresh;
use ltk_manager_runtime::patcher::session::{self, SessionError, SessionObserver};
use ltk_manager_runtime::patcher::state::{PatcherPhase, PatcherStateInner, StoredPatcherConfig};

/// How long a rebuild waits for the last game's process to let go of the overlay.
const GAME_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a rebuild checks whether the game process is gone.
const GAME_RELEASE_POLL: Duration = Duration::from_millis(100);

/// The workshop projects a session started from `config` tests.
pub fn workshop_tests(config: &StoredPatcherConfig) -> Vec<WorkshopTestProject> {
    let layers = config.workshop_layers.as_ref();

    config
        .workshop_projects
        .iter()
        .flatten()
        .map(|path| WorkshopTestProject {
            path: PathBuf::from(path),
            enabled_layers: layers
                .and_then(|layers| layers.get(path))
                .map(|names| names.iter().cloned().collect()),
        })
        .collect()
}

/// The host that a session runs and the identity of its binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedHost {
    pub exe: PathBuf,
    pub binaries: PatcherBinaries,
    /// The advisories that the delivery of the patcher raised. Each build of the session
    /// records them.
    pub advisories: Advisories,
    /// The delivered build, kept so an elevated session can stage it without downloading it
    /// again.
    pub delivery: StagedPatcher,
}

/// Prepares the host of a session on the session thread, because it can wait for a download.
pub type PrepareHost = Box<dyn FnOnce() -> AppResult<PreparedHost> + Send>;

/// Stages the prepared host where an elevated session may load it.
///
/// An elevated session runs this once, after the first overlay build, so no UAC prompt appears
/// before the work that can fail without one. Returns the host whose binaries are staged.
pub type StageHost = Box<dyn FnOnce(PreparedHost) -> AppResult<PreparedHost> + Send>;

/// Per-session inputs for [`PatcherThread::start`], resolved by the caller
/// before the patcher state is claimed.
pub struct SessionParams {
    pub prepare_host: PrepareHost,
    /// The staging step of a session that runs its host at high integrity, or `None` when it
    /// runs at the manager's integrity.
    pub stage_host: Option<StageHost>,
    pub config: Config,
    pub library: ModLibrary,
    pub workshop_projects: Vec<WorkshopTestProject>,
    pub host_flags: u32,
    /// Whether the session runs its host at high integrity through the UAC bridge.
    pub should_elevate: bool,
    /// Where the session's incidents and game records are written.
    pub stores: SessionStores,
    /// Whether the overlay is built from scratch rather than reused.
    pub force_rebuild: bool,
    /// Where the session reports that it ended.
    pub telemetry: Telemetry,
}

/// Inputs moved into the background patcher thread.
pub struct PatcherThread {
    events: Arc<dyn PatcherEvents>,
    observer: Arc<SessionObserver>,
    state: Arc<Mutex<PatcherStateInner>>,
    host: Arc<Mutex<Option<PatcherHost>>>,
    stop_flag: Arc<AtomicBool>,
    refresh: Arc<OverlayRefresh>,
    config: Config,
    library: ModLibrary,
    workshop_projects: Vec<WorkshopTestProject>,
    host_flags: u32,
    should_elevate: bool,
    stage_host: Option<StageHost>,
    force_rebuild: bool,
    telemetry: Telemetry,
}

/// Builds the first overlay and then stages the host.
///
/// An elevated session stages its host here rather than before the build, so the UAC
/// prompt appears only after the build succeeded and a failed build raises no prompt.
/// Returns `None` if the build failed or the caller stopped the session first.
fn build_and_stage(
    build: impl FnOnce() -> Option<String>,
    host: PreparedHost,
    stage: Option<StageHost>,
) -> AppResult<Option<(PreparedHost, String)>> {
    let Some(overlay_prefix) = build() else {
        return Ok(None);
    };

    let host = match stage {
        Some(stage) => stage(host)?,
        None => host,
    };

    Ok(Some((host, overlay_prefix)))
}

impl PatcherThread {
    /// Atomically claim the patcher state and spawn the session thread.
    ///
    /// The running check and every start side effect (stop-flag reset, phase,
    /// config stash for hot-reload, the `Building` notification, handle store)
    /// happen under one lock, so a concurrent start can't slip between them and
    /// respawn the shared host under a live session.
    pub fn start(
        events: Arc<dyn PatcherEvents>,
        state: &Arc<Mutex<PatcherStateInner>>,
        host: &Arc<Mutex<Option<PatcherHost>>>,
        stored_config: StoredPatcherConfig,
        params: SessionParams,
    ) -> AppResult<()> {
        let mut patcher_state = state.lock();
        if patcher_state.is_running() {
            return Err(PatcherError::AlreadyRunning.into());
        }

        let origin = stored_config.origin();
        patcher_state.stop_flag.store(false, Ordering::SeqCst);
        patcher_state.begin_session(origin.clone());
        patcher_state.last_config = Some(stored_config);

        // Announced under the same lock so the session's failure path (which
        // resets the phase after locking the state) can't be observed before
        // the flip to `Building`.
        events.phase_changed(PatcherPhase::Building);

        let SessionParams {
            prepare_host,
            stage_host,
            config,
            library,
            workshop_projects,
            host_flags,
            should_elevate,
            stores,
            force_rebuild,
            telemetry,
        } = params;
        let pipeline = Arc::new(IncidentPipeline::new(
            config.clone(),
            host_flags,
            library.clone(),
            workshop_projects.clone(),
            stores,
            Arc::clone(&events),
            telemetry.clone(),
        ));
        // The log lines carry the session from here until the session thread ends.
        let scope = LogIds::global().begin_session();
        let observer = Arc::new(SessionObserver::new(
            Arc::clone(&events),
            GameRecorder::new(origin, scope.ids()),
            Arc::new(move |record| pipeline.spawn(record)),
        ));
        let session = Self {
            events,
            observer,
            state: Arc::clone(state),
            host: Arc::clone(host),
            stop_flag: Arc::clone(&patcher_state.stop_flag),
            refresh: Arc::clone(&patcher_state.overlay_refresh),
            config,
            library,
            workshop_projects,
            host_flags,
            should_elevate,
            stage_host,
            force_rebuild,
            telemetry,
        };
        patcher_state.thread_handle = Some(thread::spawn(move || {
            let _scope = scope;
            session.run(prepare_host);
        }));
        Ok(())
    }

    fn run(mut self, prepare_host: PrepareHost) {
        let prepared = match prepare_host() {
            Ok(host) => host,
            Err(e) => {
                tracing::error!(error = ?e, "Could not prepare the patcher host");
                self.record_failure(&e);
                self.report_prepare_failure(&e);
                self.events.error(e);
                self.reset_to_idle();
                return;
            }
        };
        let host_advisories = prepared.advisories.clone();
        self.observer.set_patcher(prepared.binaries.clone());

        if self.stop_flag.load(Ordering::SeqCst) {
            tracing::info!("Stop requested while preparing the patcher host, exiting");
            self.reset_to_idle();
            return;
        }

        let force_rebuild = self.force_rebuild;
        let stage_host = self.stage_host.take();
        let first = build_and_stage(
            || self.build_overlay(force_rebuild, 0, &host_advisories),
            prepared,
            stage_host,
        );
        let (host, mut overlay_prefix) = match first {
            Ok(Some(built)) => built,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = ?e, "Could not stage the elevated patcher host");
                self.record_failure(&e);
                self.report_prepare_failure(&e);
                self.events.error(e);
                self.reset_to_idle();
                return;
            }
        };
        // A staged host has its own binaries; the delivered ones were recorded for the gap
        // before staging.
        self.observer.set_patcher(host.binaries.clone());

        let mut rebuilds = 0u64;

        loop {
            if self.run_session(&host.exe, overlay_prefix) == SessionEnd::Stopped {
                break;
            }
            rebuilds += 1;
            self.resume_building();

            // The staged host is reused across rebuilds, so no rebuild raises a UAC prompt.
            let Some(next) = self.build_overlay(false, rebuilds, &host_advisories) else {
                return;
            };
            overlay_prefix = next;
        }

        self.reset_to_idle();
        tracing::info!("Patcher thread exiting");
    }

    /// Keeps the domain failure of a start for a status reader after the thread ends.
    fn record_failure(&self, error: &AppError) {
        if let Some(error) = error.domain::<PatcherError>() {
            self.state.lock().record_error(error.clone());
        }
    }

    /// Reports a start that got no host, by the delivery or staging failure's kind.
    ///
    /// A failure that is not a `DllUnavailable` or a `StagingRefused` reports as `other`.
    fn report_prepare_failure(&self, error: &AppError) {
        let reason = match error.domain::<PatcherError>() {
            Some(PatcherError::DllUnavailable { reason }) => reason.as_str(),
            Some(PatcherError::StagingRefused { .. }) => "staging-refused",
            _ => "other",
        };
        self.telemetry.track(
            telemetry::PATCHER_START_FAILED,
            telemetry::patcher_start_failed(self.library.app_version(), reason),
        );
    }

    /// Build the overlay and return its prefix path, or `None` on failure or
    /// an early stop (state already reset). `rebuilds` counts the builds the
    /// session ran before this one. The record of the build holds the advisories of the
    /// build and `host_advisories`.
    fn build_overlay(
        &self,
        force_rebuild: bool,
        rebuilds: u64,
        host_advisories: &Advisories,
    ) -> Option<String> {
        self.refresh.clear();

        let stop_flag = Arc::clone(&self.stop_flag);
        let build = match self.library.ensure_overlay(
            &self.config,
            &self.workshop_projects,
            force_rebuild,
            move || stop_flag.load(Ordering::SeqCst),
        ) {
            Ok(build) => build,
            Err(AppError::Overlay(ltk_overlay::Error::CalledOff)) => {
                tracing::info!("Stop requested during overlay build, exiting");
                self.reset_to_idle();
                return None;
            }
            Err(e) => {
                tracing::error!(error = ?e, "Overlay build failed");
                let message = message_with_sources(&e);
                self.state.lock().record_error(PatcherError::BuildFailed {
                    message: message.clone(),
                });
                self.observer.session_failed(SessionFailure::Build {
                    kind: e.kind(),
                    category: e.overlay_category(),
                    message,
                });
                self.events.error(e);
                self.reset_to_idle();
                return None;
            }
        };

        let OverlayBuild {
            overlay_root,
            outcome,
            timings,
            mut record,
        } = build;
        record.advisories.merge(host_advisories.clone());
        self.observer
            .set_overlay_fingerprint(record.fingerprint.clone());
        self.report_overlay_built(&timings, rebuilds);
        // Refreshes the library badges and the advisories. It happens even if we are
        // about to bail on a stop request - the data is already correct.
        self.library
            .record_overlay_build(&self.config, outcome, &record);

        if self.stop_flag.load(Ordering::SeqCst) {
            tracing::info!("Stop requested after overlay build, exiting");
            self.reset_to_idle();
            return None;
        }

        tracing::info!("Using overlay root: {}", overlay_root.display());
        Some(session::normalize_overlay_prefix(
            &overlay_root.display().to_string(),
        ))
    }

    /// Run one injection session via the core orchestration, blocking until the
    /// caller stops or a library edit asks for a rebuild. A failure is reported
    /// and ends the session.
    ///
    /// The session claims the patching phase only once the host answers its commands, so
    /// an embedder that polls the status never sees `patching` while the UAC prompt of an
    /// elevated host is still open. A dismissed prompt ends the start before the claim.
    fn run_session(&self, host_exe: &Path, overlay_prefix: String) -> SessionEnd {
        let host_config = HostConfig {
            prefix: overlay_prefix.clone(),
            log_level: if self.config.verbose_patcher_logging {
                HostLogLevel::Debug
            } else {
                HostLogLevel::Info
            },
            flags: self.host_flags,
        };

        let started = session::start_injection_host(
            &self.host,
            host_exe,
            self.should_elevate,
            &host_config,
            &self.stop_flag,
        );

        let started = match started {
            Ok(Some(started)) => started,
            Ok(None) => {
                tracing::info!("Stop requested while starting the injection host");
                return SessionEnd::Stopped;
            }
            Err(e) => return self.fail_session(SessionError::Host(e)),
        };

        self.enter_patching(overlay_prefix);

        let result = session::run_injection_session(
            started,
            &self.host,
            self.should_elevate,
            &self.stop_flag,
            &self.refresh,
            Arc::clone(&self.observer),
        );

        match result {
            Ok(SessionEnd::Refresh) => SessionEnd::Refresh,
            Ok(SessionEnd::Stopped) => {
                tracing::info!("Injector stopped");
                self.observer.session_stopped();
                SessionEnd::Stopped
            }
            Err(e) => self.fail_session(e),
        }
    }

    /// Reports a session that could not start or run, and ends it.
    fn fail_session(&self, error: SessionError) -> SessionEnd {
        match &error {
            SessionError::Host(err) => tracing::error!("Failed to start injection host: {}", err),
            SessionError::Injector(err) => tracing::error!("Injector error: {}", err),
        }
        let error = PatcherError::from(error);
        if let PatcherError::InjectionFailed { stage, message } = &error {
            self.observer.session_failed(SessionFailure::Injection {
                stage: *stage,
                message: message.clone(),
            });
        }
        self.state.lock().record_error(error.clone());
        self.events.error(AppError::from(error));
        SessionEnd::Stopped
    }

    /// Reports how long a build took, by the bucket of each part.
    fn report_overlay_built(&self, timings: &OverlayTimings, rebuilds: u64) {
        let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let facts = telemetry::OverlayBuildFacts {
            rebuild: rebuilds > 0,
            rebuilds,
            prepare_secs: timings.prepare.as_secs(),
            build_secs: timings.build.as_secs(),
            builtins: count(timings.builtins.len()),
            builtins_reused: count(timings.builtins.iter().filter(|t| t.reused).count()),
        };
        self.telemetry.track(
            telemetry::OVERLAY_BUILT,
            telemetry::overlay_built(self.library.app_version(), &facts),
        );
    }

    /// Return to the build phase for a rebuild, once the last game's process has
    /// let go of the overlay or the wait runs out.
    fn resume_building(&self) {
        self.state.lock().resume_building();
        self.events.phase_changed(PatcherPhase::Building);

        if is_game_running() {
            self.events
                .overlay_progress(OverlayProgress::preparing(PrepareStep::GameExit));
        }
        let deadline = Instant::now() + GAME_RELEASE_TIMEOUT;
        while is_game_running() && !self.stop_flag.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                tracing::warn!("Game still running, rebuilding the overlay anyway");
                break;
            }
            thread::sleep(GAME_RELEASE_POLL);
        }
    }

    /// Move the session to patching against the overlay the build produced.
    ///
    /// Records the transition, then announces it - in that order, so an
    /// embedder that reads the shared state from its listener never sees the
    /// old value.
    fn enter_patching(&self, overlay_prefix: String) {
        self.state.lock().enter_patching(overlay_prefix);
        self.events.phase_changed(PatcherPhase::Patching);
    }

    /// Close the session. Runs on every thread exit path.
    fn reset_to_idle(&self) {
        self.state.lock().end_session();
        self.events.phase_changed(PatcherPhase::Idle);
    }
}

#[cfg(test)]
mod tests;
