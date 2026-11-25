use super::*;

fn elevated_facts() -> ElevationFacts {
    ElevationFacts {
        elevated: true,
        elevation_type: ElevationType::Full,
        integrity: Some(HIGH_INTEGRITY),
        shell_elevated: Some(false),
        parent: Some("explorer.exe".to_string()),
        uac_enabled: true,
        builtin_administrator: false,
        compat_layer: None,
    }
}

#[test]
fn cause_is_none_if_process_is_not_elevated() {
    let facts = ElevationFacts {
        elevated: false,
        uac_enabled: false,
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), None);
}

#[test]
fn cause_names_run_as_administrator_for_a_full_token_from_the_shell() {
    assert_eq!(
        elevated_facts().cause(),
        Some(ElevationCause::RunAsAdministrator)
    );
}

#[test]
fn cause_names_uac_before_every_other_fact() {
    let facts = ElevationFacts {
        uac_enabled: false,
        builtin_administrator: true,
        compat_layer: Some("HKCU".to_string()),
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::UacDisabled));
}

#[test]
fn cause_names_the_builtin_administrator_account() {
    let facts = ElevationFacts {
        elevation_type: ElevationType::Default,
        builtin_administrator: true,
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::BuiltinAdministrator));
}

#[test]
fn cause_names_a_compat_layer_before_the_shell() {
    let facts = ElevationFacts {
        compat_layer: Some("HKLM".to_string()),
        shell_elevated: Some(true),
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::CompatLayer));
}

#[test]
fn cause_names_an_elevated_shell() {
    let facts = ElevationFacts {
        shell_elevated: Some(true),
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::ElevatedShell));
}

#[test]
fn cause_names_an_elevated_parent_that_is_not_the_shell() {
    let facts = ElevationFacts {
        parent: Some("WindowsTerminal.exe".to_string()),
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::ElevatedParent));
}

#[test]
fn cause_is_unknown_if_the_token_is_not_full_and_nothing_else_matches() {
    let facts = ElevationFacts {
        elevation_type: ElevationType::Unknown,
        parent: None,
        ..elevated_facts()
    };

    assert_eq!(facts.cause(), Some(ElevationCause::Unknown));
}

#[test]
fn integrity_name_names_the_known_levels_and_spells_the_rest_in_hex() {
    let high = ElevationFacts {
        integrity: Some(HIGH_INTEGRITY),
        ..ElevationFacts::default()
    };
    let medium = ElevationFacts {
        integrity: Some(MEDIUM_INTEGRITY),
        ..ElevationFacts::default()
    };
    let other = ElevationFacts {
        integrity: Some(0x4000),
        ..ElevationFacts::default()
    };

    assert_eq!(high.integrity_name().as_deref(), Some("high"));
    assert_eq!(medium.integrity_name().as_deref(), Some("medium"));
    assert_eq!(other.integrity_name().as_deref(), Some("0x4000"));
    assert_eq!(ElevationFacts::default().integrity_name(), None);
}

#[test]
fn cause_and_type_serialize_in_kebab_case() {
    assert_eq!(
        serde_json::to_value(ElevationCause::RunAsAdministrator).unwrap(),
        serde_json::json!("run-as-administrator")
    );
    assert_eq!(ElevationCause::UacDisabled.to_string(), "uac-disabled");
    assert_eq!(ElevationType::Full.to_string(), "full");
}

fn league_process_elevated_when(pid: Option<u32>, elevated: Option<bool>, denied: bool) -> bool {
    league_process_elevated_from(
        &|name| {
            if name == "leagueclientux.exe" {
                pid
            } else {
                None
            }
        },
        &|_| elevated,
        &|_| denied,
    )
}

#[test]
fn league_process_elevated_is_false_without_a_league_process() {
    assert!(!league_process_elevated_when(None, None, false));
}

#[test]
fn league_process_elevated_is_true_when_the_client_token_is_elevated() {
    assert!(league_process_elevated_when(Some(1234), Some(true), false));
}

#[test]
fn league_process_elevated_is_false_when_the_client_token_is_not_elevated() {
    assert!(!league_process_elevated_when(
        Some(1234),
        Some(false),
        false
    ));
}

/// An elevated process refuses the open, so the refusal answers the question the token
/// read could not.
#[test]
fn league_process_elevated_is_true_when_the_open_is_denied() {
    assert!(league_process_elevated_when(Some(1234), None, true));
}

#[test]
fn league_process_elevated_checks_the_client_before_the_game() {
    let looked_up = std::cell::RefCell::new(Vec::new());
    let elevated = league_process_elevated_from(
        &|name| {
            looked_up.borrow_mut().push(name.to_string());
            (name == "league of legends.exe").then_some(99)
        },
        &|_| Some(true),
        &|_| false,
    );

    assert!(elevated);
    assert_eq!(
        looked_up.borrow().first().map(String::as_str),
        Some("leagueclientux.exe")
    );
}
