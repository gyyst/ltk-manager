//! Reads the WebView2 browser process and refuses a debugging switch.

use super::{refused_debug_switch, without_value};

/// The telemetry code of a browser process that [`verify_browser_process`] could not read.
const UNREADABLE_CODE: &str = "WEBVIEW_CHECK_UNREADABLE";

/// Closes the manager if its WebView2 browser process runs with a debugging switch, or if a
/// registry policy replaces the WebView2 runtime folder.
///
/// WebView2 appends the registry policy and the variable that a library loaded into this
/// process set again, so the check reads the command line of the browser process. A start by
/// the Sona LTF launcher (`sona_ltf`) keeps its debugging switches. The check runs on the
/// webview's thread once the controller exists.
///
/// # Errors
///
/// Fails if the webview cannot run the check, which `with_webview` reports.
pub fn verify_browser_process(window: &tauri::WebviewWindow, sona_ltf: bool) -> tauri::Result<()> {
    use tauri::Manager;

    let app = window.app_handle().clone();
    window.with_webview(move |webview| {
        if let Some(name) = process::executable_folder_policy() {
            tracing::error!(
                %name,
                "A BrowserExecutableFolder policy replaces the WebView2 runtime; closing"
            );
            process::alert(&format!(
                "A registry policy replaces the WebView2 Runtime of LTK Manager, and LTK Manager \
                 will close.\n\nRemove the value \"{name}\" from the registry key {}, then start \
                 LTK Manager again.",
                process::EXECUTABLE_FOLDER_POLICY
            ));
            app.exit(1);
            return;
        }

        let mut pid = 0u32;
        // SAFETY: `with_webview` runs on the thread that owns the controller, and `pid`
        // outlives the call.
        let read = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.BrowserProcessId(&mut pid))
        };
        if let Err(error) = read {
            tracing::warn!(%error, "Could not read the WebView2 browser process id");
            crate::telemetry::report_app_error(UNREADABLE_CODE, Some("PID"), &error.to_string());
            return;
        }
        let Some(command_line) = process::command_line(pid) else {
            tracing::warn!(pid, "Could not read the browser process's command line");
            crate::telemetry::report_app_error(
                UNREADABLE_CODE,
                Some("COMMAND_LINE"),
                "The command line of the WebView2 browser process could not be read",
            );
            return;
        };
        let Some(switch) = refused_debug_switch(&command_line, sona_ltf) else {
            tracing::debug!(
                pid,
                sona_ltf,
                "The browser process keeps no refused debugging switch"
            );
            return;
        };

        tracing::error!(
            pid,
            switch = without_value(&switch),
            "The WebView2 browser process runs with a debugging switch; closing"
        );
        process::alert(&format!(
            "LTK Manager was started with a WebView2 debugging switch ({switch}) and will \
             close.\n\nStart it from the Start menu or the desktop, not through another program."
        ));
        app.exit(1);
    })
}

/// Reads another process's command line and the runtime folder policy, and tells the user
/// about a refused one.
mod process {
    use std::ptr::null_mut;

    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, STATUS_INFO_LENGTH_MISMATCH, UNICODE_STRING,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    /// The registry key whose values replace the WebView2 runtime folder of an app.
    pub const EXECUTABLE_FOLDER_POLICY: &str =
        r"Software\Policies\Microsoft\Edge\WebView2\BrowserExecutableFolder";

    /// Returns the name of the [`EXECUTABLE_FOLDER_POLICY`] value that WebView2 reads for this
    /// process. Returns `None` if no value applies.
    ///
    /// WebView2 reads HKLM, then HKCU.
    pub fn executable_folder_policy() -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let exe = exe.file_name()?.to_str()?;
        let app_id = explicit_app_id();

        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .into_iter()
            .find_map(|root| {
                let key = RegKey::predef(root)
                    .open_subkey(EXECUTABLE_FOLDER_POLICY)
                    .ok()?;
                let name = key
                    .enum_values()
                    .filter_map(Result::ok)
                    .map(|(name, _)| name)
                    .find(|name| policy_applies(name, exe, app_id.as_deref()))?;
                let folder: String = key.get_value(&name).ok()?;

                (!folder.is_empty()).then_some(name)
            })
    }

    /// Returns `true` if WebView2 reads the policy value `name` for the executable `exe` with
    /// the Application User Model ID `app_id`.
    pub fn policy_applies(name: &str, exe: &str, app_id: Option<&str>) -> bool {
        name == "*"
            || name.eq_ignore_ascii_case(exe)
            || app_id.is_some_and(|app_id| name.eq_ignore_ascii_case(app_id))
    }

    /// The Application User Model ID set for this process, or `None` if none is set.
    fn explicit_app_id() -> Option<String> {
        // SAFETY: the call returns a null-terminated string, which is freed once after the copy.
        unsafe {
            let id = GetCurrentProcessExplicitAppUserModelID().ok()?;
            let text = id.to_string().ok();
            CoTaskMemFree(Some(id.as_ptr().cast()));
            text
        }
    }

    /// The command line of process `pid`, or `None` if it cannot be read.
    ///
    /// `ProcessCommandLineInformation` answers for a process of the same user with the limited
    /// query right, which a child of this process grants.
    pub fn command_line(pid: u32) -> Option<String> {
        // SAFETY: the call takes no pointer, and a null handle is checked before use.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }

        let mut needed = 0u32;
        // SAFETY: a zero-length query writes only `needed`.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                null_mut(),
                0,
                &mut needed,
            )
        };
        let line = if status == STATUS_INFO_LENGTH_MISMATCH && needed > 0 {
            read_command_line(process, needed)
        } else {
            None
        };

        // SAFETY: `process` is the open handle this function owns, closed once.
        unsafe { CloseHandle(process) };
        line
    }

    /// Reads `needed` bytes of `ProcessCommandLineInformation` from `process`.
    fn read_command_line(
        process: windows_sys::Win32::Foundation::HANDLE,
        needed: u32,
    ) -> Option<String> {
        let needed_len = usize::try_from(needed).ok()?;
        // `u64` cells align the leading `UNICODE_STRING`, whose `Buffer` is a pointer.
        let mut buffer = vec![0u64; needed_len.div_ceil(size_of::<u64>())];
        let mut written = 0u32;
        // SAFETY: `buffer` holds at least `needed` bytes, is 8-aligned, and outlives the call.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut written,
            )
        };
        if status < 0 || buffer.is_empty() {
            return None;
        }

        // SAFETY: on success the buffer starts with a `UNICODE_STRING`, which the alignment of
        // `buffer` and the length check above make readable.
        let string = unsafe { buffer.as_ptr().cast::<UNICODE_STRING>().read() };
        let bytes = usize::from(string.Length);
        let start = buffer.as_ptr() as usize;
        let text = string.Buffer as usize;
        // The characters follow the header inside the same buffer. Anything else is refused.
        if text < start + size_of::<UNICODE_STRING>()
            || !text.is_multiple_of(align_of::<u16>())
            || text.checked_add(bytes)? > start + needed_len
        {
            return None;
        }
        // SAFETY: `Buffer` points at `Length` bytes of UTF-16 inside `buffer`, checked above,
        // and `buffer` is alive and unchanged for the read.
        let wide = unsafe { std::slice::from_raw_parts(string.Buffer, bytes / 2) };
        Some(String::from_utf16_lossy(wide))
    }

    /// Shows `text` in an error box and waits for it to be dismissed.
    pub fn alert(text: &str) {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (text, title) = (wide(text), wide("LTK Manager"));
        // SAFETY: both strings are null-terminated and outlive the call.
        unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            )
        };
    }
}
