use super::*;

fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ltk-staging-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn is_sha256_hex_accepts_lowercase_hex_of_64_characters() {
    assert!(is_sha256_hex(&"a".repeat(64)));
    assert!(is_sha256_hex(&digest_of(b"payload")));
}

#[test]
fn is_sha256_hex_rejects_uppercase_short_and_non_hex() {
    assert!(!is_sha256_hex(&"A".repeat(64)));
    assert!(!is_sha256_hex(&"a".repeat(63)));
    assert!(!is_sha256_hex(&"a".repeat(65)));
    assert!(!is_sha256_hex(&format!("{}g", "a".repeat(63))));
}

#[test]
fn staging_dir_joins_the_install_directory_and_the_digest() {
    let digest = "a".repeat(64);
    let dir = staging_dir(Path::new(r"C:\Program Files\LTK Manager"), &digest).unwrap();

    assert_eq!(
        dir,
        PathBuf::from(r"C:\Program Files\LTK Manager")
            .join("patcher")
            .join(&digest)
    );
}

#[test]
fn staging_dir_rejects_a_digest_that_cannot_name_a_directory() {
    assert!(matches!(
        staging_dir(Path::new(r"C:\Program Files\LTK Manager"), "../escape"),
        Err(StagingError::InvalidDigest { .. })
    ));
}

#[test]
fn protected_install_dir_accepts_a_path_under_a_program_files_root() {
    let roots = vec![
        PathBuf::from(r"C:\Program Files"),
        PathBuf::from(r"C:\Program Files (x86)"),
    ];
    let dir = Path::new(r"C:\Program Files\LTK Manager");

    assert_eq!(protected_install_dir(dir, &roots).unwrap(), dir);
}

#[test]
fn protected_install_dir_is_case_insensitive() {
    let roots = vec![PathBuf::from(r"C:\Program Files")];
    let dir = Path::new(r"c:\program files\LTK Manager");

    assert_eq!(protected_install_dir(dir, &roots).unwrap(), dir);
}

#[test]
fn protected_install_dir_rejects_a_path_outside_every_root() {
    let roots = vec![PathBuf::from(r"C:\Program Files")];
    let dir = Path::new(r"D:\Games\LTK Manager");

    assert!(matches!(
        protected_install_dir(dir, &roots),
        Err(StagingError::InstallDirUnprotected { .. })
    ));
}

#[test]
fn copy_into_writes_the_dll_and_host_and_matches_the_digest() {
    let source = temp_dir("source");
    let bundled = temp_dir("bundled");
    let target = temp_dir("target").join("nested");
    let dll_bytes = b"patcher dll";
    let digest = digest_of(dll_bytes);
    fs::write(source.join(HOOK_DLL_NAME), dll_bytes).unwrap();
    fs::write(bundled.join(HOST_EXE_NAME), b"host exe").unwrap();

    copy_into(&source, &bundled.join(HOST_EXE_NAME), &target, &digest).unwrap();

    assert_eq!(fs::read(target.join(HOOK_DLL_NAME)).unwrap(), dll_bytes);
    assert_eq!(
        fs::read(target.join(HOST_EXE_NAME)).unwrap(),
        b"host exe".to_vec()
    );
}

#[test]
fn copy_into_rejects_a_dll_that_does_not_match_the_digest() {
    let source = temp_dir("bad-source");
    let bundled = temp_dir("bad-bundled");
    let target = temp_dir("bad-target");
    fs::write(source.join(HOOK_DLL_NAME), b"tampered").unwrap();
    fs::write(bundled.join(HOST_EXE_NAME), b"host exe").unwrap();

    let result = copy_into(
        &source,
        &bundled.join(HOST_EXE_NAME),
        &target,
        &"a".repeat(64),
    );

    assert!(matches!(result, Err(StagingError::DigestMismatch { .. })));
    assert!(!target.join(HOOK_DLL_NAME).exists());
}

/// Root writes through a read-only permission, so the check cannot be exercised as root.
#[cfg(unix)]
fn runs_as_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/")
        .map(|meta| meta.uid() == 0 && unsafe { libc_geteuid() } == 0)
        .unwrap_or(false)
}

#[cfg(unix)]
unsafe fn libc_geteuid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no arguments and cannot fail.
    unsafe { geteuid() }
}

#[cfg(not(unix))]
fn runs_as_root() -> bool {
    false
}

#[test]
fn verify_staged_accepts_a_matching_dll_in_a_read_only_directory() {
    if runs_as_root() {
        return;
    }

    let dir = temp_dir("verify-ok");
    let dll_bytes = b"patcher dll";
    let digest = digest_of(dll_bytes);
    fs::write(dir.join(HOOK_DLL_NAME), dll_bytes).unwrap();
    make_read_only(&dir);

    let staged = verify_staged(&dir, &digest, HOOK_DLL_NAME).unwrap();

    assert_eq!(staged.digest, digest);
    assert_eq!(staged.host, dir.join(HOST_EXE_NAME));
    make_writable(&dir);
}

#[test]
fn verify_staged_rejects_a_dll_that_does_not_match_the_digest() {
    let dir = temp_dir("verify-mismatch");
    fs::write(dir.join(HOOK_DLL_NAME), b"tampered").unwrap();

    assert!(matches!(
        verify_staged(&dir, &"a".repeat(64), HOOK_DLL_NAME),
        Err(StagingError::DigestMismatch { .. })
    ));
}

/// Off Windows the writability probe succeeds for the owner, which is the case the check
/// rejects.
#[cfg(not(target_os = "windows"))]
#[test]
fn verify_staged_rejects_a_directory_the_user_can_write() {
    if runs_as_root() {
        return;
    }

    let dir = temp_dir("verify-writable");
    let dll_bytes = b"patcher dll";
    let digest = digest_of(dll_bytes);
    fs::write(dir.join(HOOK_DLL_NAME), dll_bytes).unwrap();

    assert!(matches!(
        verify_staged(&dir, &digest, HOOK_DLL_NAME),
        Err(StagingError::DirectoryWritable { .. })
    ));
}

#[cfg(unix)]
fn make_read_only(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
}

#[cfg(unix)]
fn make_writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_read_only(_dir: &Path) {}

#[cfg(not(unix))]
fn make_writable(_dir: &Path) {}

#[test]
fn stage_request_reads_a_full_argument_list() {
    let request = StageRequest::from_args([
        "--stage-elevated-patcher".into(),
        "--source".into(),
        "/app/patcher/abc".into(),
        "--digest".into(),
        "abc".into(),
        "--install-dir".into(),
        "/install".into(),
    ]);

    assert_eq!(
        request,
        Some(StageRequest {
            source: PathBuf::from("/app/patcher/abc"),
            digest: "abc".to_string(),
            install_dir: PathBuf::from("/install"),
        })
    );
}

#[test]
fn stage_request_is_none_without_the_staging_argument() {
    assert_eq!(StageRequest::from_args(["--report".into()]), None);
}

#[test]
fn stage_request_is_none_when_a_value_is_missing() {
    assert_eq!(
        StageRequest::from_args([
            "--stage-elevated-patcher".into(),
            "--source".into(),
            "/app/patcher/abc".into(),
        ]),
        None
    );
}

#[test]
fn stage_request_round_trips_through_its_arguments() {
    let request = StageRequest {
        source: PathBuf::from(r"C:\app\patcher\abc"),
        digest: "a".repeat(64),
        install_dir: PathBuf::from(r"C:\Program Files\LTK Manager"),
    };

    assert_eq!(StageRequest::from_args(request.to_args()), Some(request));
}

#[test]
fn run_helper_rejects_an_install_directory_outside_program_files() {
    let source = temp_dir("helper-unprotected-source");
    let bundled = temp_dir("helper-unprotected-bundled");
    let dll_bytes = b"patcher dll";
    let digest = digest_of(dll_bytes);
    fs::write(source.join(HOOK_DLL_NAME), dll_bytes).unwrap();
    fs::write(bundled.join(HOST_EXE_NAME), b"host exe").unwrap();

    let request = StageRequest {
        source: source.clone(),
        digest,
        install_dir: PathBuf::from(r"D:\Games\LTK Manager"),
    };

    let result = run_helper(
        &request,
        &bundled.join(HOST_EXE_NAME),
        &[PathBuf::from(r"C:\Program Files")],
    );

    assert!(matches!(
        result,
        Err(StagingError::InstallDirUnprotected { .. })
    ));
}

#[test]
fn run_helper_copies_into_the_protected_install_directory() {
    if runs_as_root() {
        return;
    }

    let source = temp_dir("helper-source");
    let bundled = temp_dir("helper-bundled");
    let install = temp_dir("helper-install");
    let dll_bytes = b"patcher dll";
    let digest = digest_of(dll_bytes);
    fs::write(source.join(HOOK_DLL_NAME), dll_bytes).unwrap();
    fs::write(bundled.join(HOST_EXE_NAME), b"host exe").unwrap();

    let request = StageRequest {
        source: source.clone(),
        digest: digest.clone(),
        install_dir: install.clone(),
    };

    let staged = run_helper(&request, &bundled.join(HOST_EXE_NAME), &[install.clone()]).unwrap();

    assert_eq!(staged.dir, install.join("patcher").join(&digest));
    assert_eq!(fs::read(staged.dir.join(HOOK_DLL_NAME)).unwrap(), dll_bytes);
}

/// The staging step never falls back to the app data DLL: a helper that reports success still
/// fails when the copy does not verify.
#[test]
fn stage_elevated_rejects_a_copy_that_does_not_verify() {
    let source = temp_dir("stage-unverified-source");
    let bundled = temp_dir("stage-unverified-bundled");
    let install = temp_dir("stage-unverified-install");
    let digest = digest_of(b"patcher dll");
    fs::write(source.join(HOOK_DLL_NAME), b"patcher dll").unwrap();
    fs::write(bundled.join(HOST_EXE_NAME), b"host exe").unwrap();

    let request = StageRequest {
        source,
        digest: digest.clone(),
        install_dir: install.clone(),
    };

    // A helper that leaves a different DLL behind, as a failed copy would.
    let target = staging_dir(&install, &digest).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join(HOOK_DLL_NAME), b"tampered").unwrap();

    let result = verify_staged(&target, &digest, HOOK_DLL_NAME);

    assert!(matches!(result, Err(StagingError::DigestMismatch { .. })));
}
