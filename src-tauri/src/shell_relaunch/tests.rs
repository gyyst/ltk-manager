use super::*;

fn skip(line: &str) -> String {
    let line: Vec<u16> = line.encode_utf16().collect();
    String::from_utf16(skip_program_name(&line)).unwrap()
}

#[test]
fn skip_program_name_follows_command_line_to_argv() {
    assert_eq!(
        skip(r#""C:\Program Files\LTK\ltk-manager.exe" "ltk://x y" a"#),
        r#""ltk://x y" a"#
    );
    assert_eq!(skip(r"C:\ltk\ltk-manager.exe   a b"), "a b");
    assert_eq!(skip(r#""C:\ltk\ltk-manager.exe""#), "");
    assert_eq!(skip("ltk-manager.exe"), "");
}

#[test]
fn must_relaunch_is_false_under_the_shell_without_debugging() {
    assert!(!must_relaunch(Some(7), 7, false));
}

#[test]
fn must_relaunch_is_true_under_another_parent() {
    assert!(must_relaunch(Some(8), 7, false));
    assert!(must_relaunch(None, 7, false));
}

#[test]
fn must_relaunch_is_true_under_the_shell_if_debugging() {
    assert!(must_relaunch(Some(7), 7, true));
}

#[test]
fn debugging_requires_relaunch_is_false_for_a_sona_ltf_start() {
    let sona = Inherited::new("--remote-debugging-port=9222", true, vec![], true);
    assert!(!debugging_requires_relaunch(&sona));

    let launcher = Inherited::new("--remote-debugging-port=9222", true, vec![], false);
    assert!(debugging_requires_relaunch(&launcher));

    let clean = Inherited::new("--disable-gpu", false, vec![], false);
    assert!(!debugging_requires_relaunch(&clean));
}
