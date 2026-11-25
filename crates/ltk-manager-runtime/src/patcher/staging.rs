//! Stages the host and its DLL in the administrator-only install directory for an elevated
//! session.
//!
//! ADR-0074 requires that a high-integrity host loads only binaries from a directory the user
//! cannot write. The delivered DLL sits under `<app data>/patcher/<digest>`, which the user
//! owns, so an elevated session copies the verified host and DLL into
//! `<install dir>/patcher/<digest>` through a UAC prompt, then verifies the copy before it
//! runs.
//!
//! The copy runs in a helper of this executable, `--stage-elevated-patcher`, which holds the
//! high-integrity token that the UAC prompt grants. The manager, still unelevated, verifies
//! the result.

use std::path::{Path, PathBuf};

use fs_err as fs;
use ltk_manager_base::hashing::sha256_file;
use thiserror::Error;

use super::host::{HOOK_DLL_NAME, HOST_EXE_NAME};

/// A directory under the install directory that holds one verified build.
const STAGING_DIR: &str = "patcher";

/// The argument that runs the staging helper of an elevated session.
pub const STAGE_ELEVATED_ARG: &str = "--stage-elevated-patcher";

/// The argument before the source directory of the helper.
pub const SOURCE_ARG: &str = "--source";

/// The argument before the digest of the helper.
pub const DIGEST_ARG: &str = "--digest";

/// The argument before the install directory of the helper.
pub const INSTALL_DIR_ARG: &str = "--install-dir";

/// A request to stage an elevated session's binaries, read from a helper's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRequest {
    /// The delivered directory that holds the DLL.
    pub source: PathBuf,
    /// SHA-256 of the DLL, lowercase hex.
    pub digest: String,
    /// The install directory that the staged copy goes under.
    pub install_dir: PathBuf,
}

impl StageRequest {
    /// Reads a request from `args`, the arguments without the program name.
    ///
    /// Returns `None` unless `args` hold [`STAGE_ELEVATED_ARG`] and one value for each of
    /// [`SOURCE_ARG`], [`DIGEST_ARG`] and [`INSTALL_DIR_ARG`].
    pub fn from_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<Self> {
        let mut staging = false;
        let mut source = None;
        let mut digest = None;
        let mut install_dir = None;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            if arg == STAGE_ELEVATED_ARG {
                staging = true;
            } else if arg == SOURCE_ARG {
                source = args.next().map(PathBuf::from);
            } else if arg == DIGEST_ARG {
                digest = args
                    .next()
                    .map(|value| value.to_string_lossy().into_owned());
            } else if arg == INSTALL_DIR_ARG {
                install_dir = args.next().map(PathBuf::from);
            }
        }

        staging.then_some(Self {
            source: source?,
            digest: digest?,
            install_dir: install_dir?,
        })
    }

    /// Returns the arguments that run a helper for this request.
    pub fn to_args(&self) -> Vec<std::ffi::OsString> {
        [
            STAGE_ELEVATED_ARG.into(),
            SOURCE_ARG.into(),
            self.source.clone().into_os_string(),
            DIGEST_ARG.into(),
            self.digest.clone().into(),
            INSTALL_DIR_ARG.into(),
            self.install_dir.clone().into_os_string(),
        ]
        .to_vec()
    }
}

/// Errors of staging an elevated session's binaries.
#[derive(Debug, Error)]
pub enum StagingError {
    /// The manager does not run from a directory under a `Program Files` root, so a copy
    /// there would be no better protected than the app data directory.
    #[error(
        "the manager does not run from a Program Files directory, so an elevated session cannot stage its binaries: {path}"
    )]
    InstallDirUnprotected { path: String },

    /// The digest is not 64 lowercase hexadecimal characters, so it cannot name a directory.
    #[error("the patcher digest is not a SHA-256 hex string: {digest}")]
    InvalidDigest { digest: String },

    /// The staged DLL does not hash to the digest that names its directory.
    #[error("the staged patcher DLL does not match its digest {digest}")]
    DigestMismatch { digest: String },

    /// The staged directory is writable by the current user, so a high-integrity host must
    /// not load from it.
    #[error("the staging directory {path} is writable by the current user")]
    DirectoryWritable { path: String },

    /// The UAC prompt was dismissed, or the helper could not start.
    #[error("staging the elevated patcher failed: {message}")]
    Helper { message: String },

    /// A filesystem operation failed.
    #[error("staging the elevated patcher failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A verified build under the administrator-only install directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedElevated {
    /// The directory that holds the host and DLL.
    pub dir: PathBuf,
    /// Path of the host executable in `dir`.
    pub host: PathBuf,
    /// SHA-256 of the DLL, lowercase hex.
    pub digest: String,
}

/// Returns whether `digest` is 64 lowercase hexadecimal characters.
pub fn is_sha256_hex(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Returns the staging directory of `digest` under `install_dir`, or an error if the digest
/// cannot name a directory.
pub fn staging_dir(install_dir: &Path, digest: &str) -> Result<PathBuf, StagingError> {
    if !is_sha256_hex(digest) {
        return Err(StagingError::InvalidDigest {
            digest: digest.to_string(),
        });
    }

    Ok(install_dir.join(STAGING_DIR).join(digest))
}

/// Returns `install_dir` if it sits under a `Program Files` root.
///
/// The roots come from the environment, so a caller passes them in and a test supplies its
/// own. A path under any root is protected by the ACL that covers the root.
pub fn protected_install_dir<'a>(
    install_dir: &'a Path,
    program_files_roots: &[PathBuf],
) -> Result<&'a Path, StagingError> {
    let under_a_root = program_files_roots.iter().any(|root| {
        install_dir
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(&root.to_string_lossy().to_ascii_lowercase())
    });

    if under_a_root {
        Ok(install_dir)
    } else {
        Err(StagingError::InstallDirUnprotected {
            path: install_dir.display().to_string(),
        })
    }
}

/// Returns whether a file can be created in `dir`, which proves the current user can write it.
///
/// A directory that refuses the create is protected, which is the answer an elevated session
/// needs. The probe file is removed before returning.
pub fn directory_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ltk-write-probe-{}", std::process::id()));
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Verifies that the DLL in `dir` hashes to `digest`, and returns the build.
///
/// The caller checks the directory ACL. This runs in both contexts: the helper checks the copy
/// it wrote, and the manager checks the copy the helper reported.
pub fn verify_digest(
    dir: &Path,
    digest: &str,
    dll_name: &str,
) -> Result<StagedElevated, StagingError> {
    let dll = dir.join(dll_name);
    let actual = sha256_file(&dll)?;
    if !actual.eq_ignore_ascii_case(digest) {
        return Err(StagingError::DigestMismatch {
            digest: digest.to_string(),
        });
    }

    Ok(StagedElevated {
        dir: dir.to_path_buf(),
        host: dir.join(HOST_EXE_NAME),
        digest: digest.to_string(),
    })
}

/// Verifies a staged build: the DLL hashes to the digest, and the directory refuses a write.
///
/// Runs in the unelevated manager, where the write probe answers whether the user can replace
/// the binaries. Returns the staged build, or an error that names the failed check.
pub fn verify_staged(
    dir: &Path,
    digest: &str,
    dll_name: &str,
) -> Result<StagedElevated, StagingError> {
    let staged = verify_digest(dir, digest, dll_name)?;

    if directory_is_writable(dir) {
        return Err(StagingError::DirectoryWritable {
            path: dir.display().to_string(),
        });
    }

    Ok(staged)
}

/// Copies the delivered host and DLL into `target` and verifies the copy.
///
/// `source_dir` holds the delivered DLL. The host comes from `bundled_host`, which the install
/// directory protects. Both copies run at high integrity, so this is the helper's work.
pub fn copy_into(
    source_dir: &Path,
    bundled_host: &Path,
    target: &Path,
    digest: &str,
) -> Result<(), StagingError> {
    let source_dll = source_dir.join(HOOK_DLL_NAME);
    let actual = sha256_file(&source_dll)?;
    if !actual.eq_ignore_ascii_case(digest) {
        return Err(StagingError::DigestMismatch {
            digest: digest.to_string(),
        });
    }

    fs::create_dir_all(target)?;
    fs::copy(&source_dll, target.join(HOOK_DLL_NAME))?;
    fs::copy(bundled_host, target.join(HOST_EXE_NAME))?;

    let copied = sha256_file(&target.join(HOOK_DLL_NAME))?;
    if !copied.eq_ignore_ascii_case(digest) {
        return Err(StagingError::DigestMismatch {
            digest: digest.to_string(),
        });
    }

    Ok(())
}

/// Runs a staging helper: copies the delivered host and DLL into the install directory and
/// verifies the copy.
///
/// Returns the verified build. Fails if the install directory is not under a `Program Files`
/// root, if the source DLL does not match `digest`, or if a copy fails.
///
/// The helper runs at high integrity, so it checks the digest of the copy but not the
/// directory ACL: an administrator can write any directory, so a write probe here would reject
/// every staging. The unelevated manager checks the ACL after the helper exits.
pub fn run_helper(
    request: &StageRequest,
    bundled_host: &Path,
    program_files_roots: &[PathBuf],
) -> Result<StagedElevated, StagingError> {
    let install_dir = protected_install_dir(&request.install_dir, program_files_roots)?;
    let target = staging_dir(install_dir, &request.digest)?;

    copy_into(&request.source, bundled_host, &target, &request.digest)?;
    verify_digest(&target, &request.digest, HOOK_DLL_NAME)
}

/// Returns the `Program Files` roots of the environment, or an empty list off Windows.
///
/// The install directory of a per-machine install sits under one of them, so a copy there
/// inherits the administrator-only ACL that covers the root.
pub fn program_files_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

/// Stages an elevated session's binaries through a UAC prompt and returns the verified build.
///
/// The copy runs in a helper of this executable, started with `runas`, which holds the
/// high-integrity token. The manager verifies the result, so a copy that the helper reports as
/// done is still rejected if its digest or its directory ACL is wrong.
///
/// Off Windows no session elevates, so this never runs. It reports the platform as unsupported
/// rather than staging an unprotected copy.
pub fn stage_elevated(
    request: &StageRequest,
    bundled_host: &Path,
    program_files_roots: &[PathBuf],
) -> Result<StagedElevated, StagingError> {
    #[cfg(target_os = "windows")]
    {
        let install_dir = protected_install_dir(&request.install_dir, program_files_roots)?;
        let target = staging_dir(install_dir, &request.digest)?;

        run_elevated_helper(request)?;
        verify_staged(&target, &request.digest, HOOK_DLL_NAME)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (request, bundled_host, program_files_roots);
        Err(StagingError::Helper {
            message: "an elevated session is unsupported on this platform".to_string(),
        })
    }
}

/// Runs this executable as a staging helper at high integrity, and waits for it.
///
/// Fails with [`StagingError::Helper`] if the UAC prompt is dismissed or the helper exits with
/// a non-zero code.
#[cfg(target_os = "windows")]
fn run_elevated_helper(request: &StageRequest) -> Result<(), StagingError> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe()?;
    let parameters = request
        .to_args()
        .iter()
        .map(|arg| format!("\"{}\"", arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");

    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&parameters);

    // SAFETY: the struct is zeroed, carries its own size, and every string it points at
    // outlives the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;

    // SAFETY: `info` is initialized and lives for the whole call.
    let launched = unsafe { ShellExecuteExW(&mut info) };
    if launched == 0 {
        return Err(StagingError::Helper {
            message: format!(
                "the elevated helper did not start, error {}",
                std::io::Error::last_os_error()
            ),
        });
    }

    if info.hProcess.is_null() {
        return Err(StagingError::Helper {
            message: "the elevated helper returned no process handle".to_string(),
        });
    }

    // SAFETY: the handle came from ShellExecuteExW and is closed below.
    let waited = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut code = 0u32;
    // SAFETY: the handle is open and `code` outlives the call.
    let read_code = unsafe { GetExitCodeProcess(info.hProcess, &mut code) };
    // SAFETY: nothing else holds the handle.
    unsafe { CloseHandle(info.hProcess) };

    if waited != WAIT_OBJECT_0 || read_code == 0 {
        return Err(StagingError::Helper {
            message: "the elevated helper did not report its result".to_string(),
        });
    }
    if code != 0 {
        return Err(StagingError::Helper {
            message: format!("the elevated helper exited with code {code}"),
        });
    }

    Ok(())
}

/// Returns `value` as a null-terminated wide string.
#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests;
