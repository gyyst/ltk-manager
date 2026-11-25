use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

/// A prepared host whose delivery is a stand-in for a staged DLL.
fn prepared_host(exe: &str) -> PreparedHost {
    PreparedHost {
        exe: PathBuf::from(exe),
        binaries: PatcherBinaries::default(),
        advisories: Advisories::default(),
        delivery: ltk_manager_runtime::patcher::delivery::StagedPatcher {
            dir: PathBuf::from("delivered"),
            version: "test".to_string(),
            digest: "0".repeat(64),
            eol: u32::MAX,
        },
    }
}

#[test]
fn workshop_tests_take_each_projects_layers() {
    let config = StoredPatcherConfig {
        workshop_projects: Some(vec!["a".to_owned(), "b".to_owned()]),
        workshop_layers: Some(HashMap::from([("a".to_owned(), vec!["extras".to_owned()])])),
    };

    let tests = workshop_tests(&config);

    assert_eq!(
        tests[0].enabled_layers,
        Some(["extras".to_owned()].into_iter().collect())
    );
    assert_eq!(tests[1].enabled_layers, None);
}

/// A build that fails must end the start without raising a UAC prompt, so the staging step sees
/// no host at all.
#[test]
fn failed_build_stages_no_host() {
    let staged = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&staged);
    let stage: StageHost = Box::new(move |host| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(host)
    });

    let result = build_and_stage(|| None, prepared_host("delivered.exe"), Some(stage));

    assert!(matches!(result, Ok(None)));
    assert_eq!(staged.load(Ordering::SeqCst), 0);
}

/// A staged session must run the host from the verified copy, and staging must happen once for
/// the build that it follows.
#[test]
fn successful_build_stages_the_host_once() {
    let staged = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&staged);
    let stage: StageHost = Box::new(move |mut host| {
        counter.fetch_add(1, Ordering::SeqCst);
        host.exe = PathBuf::from("staged.exe");
        Ok(host)
    });

    let (host, overlay_prefix) = build_and_stage(
        || Some("overlay\\".to_string()),
        prepared_host("delivered.exe"),
        Some(stage),
    )
    .unwrap()
    .unwrap();

    assert_eq!(host.exe, PathBuf::from("staged.exe"));
    assert_eq!(overlay_prefix, "overlay\\");
    assert_eq!(staged.load(Ordering::SeqCst), 1);
}

#[test]
fn unelevated_session_keeps_the_delivered_host() {
    let (host, _) = build_and_stage(
        || Some("overlay\\".to_string()),
        prepared_host("delivered.exe"),
        None,
    )
    .unwrap()
    .unwrap();

    assert_eq!(host.exe, PathBuf::from("delivered.exe"));
}

/// A refused staging ends the start with its own error, before any session runs.
#[test]
fn staging_failure_ends_the_start() {
    let stage: StageHost = Box::new(|_| {
        Err(PatcherError::StagingRefused {
            message: "install the manager under Program Files".to_string(),
        }
        .into())
    });

    let result = build_and_stage(
        || Some("overlay\\".to_string()),
        prepared_host("delivered.exe"),
        Some(stage),
    );

    match result {
        Err(error) => assert!(matches!(
            error.domain::<PatcherError>(),
            Some(PatcherError::StagingRefused { .. })
        )),
        Ok(_) => panic!("a refused staging must not return a host"),
    }
}
