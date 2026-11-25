//! Keeps WebView2 debugging switches out of the manager's browser process.
//!
//! A launcher can hand WebView2 a Chrome DevTools Protocol port through the
//! `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` environment variable, then drive the frontend from
//! outside, including answering the frontend's own commands in its place. WebView2 appends that
//! variable, and the registry policy of the same name, to the arguments the manager passes
//! through its API. So the manager reads the variable itself, keeps the switches it does not
//! object to, passes those through the API, and removes the variable before any webview exists.
//! Once the browser process runs, its command line is checked, which also covers the registry
//! and a library that put the variable back.

use std::ffi::OsStr;

/// The WebView2 variable whose switches reach the browser process, appended to the API's.
pub const BROWSER_ARGS_VAR: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// A launcher's variable naming the port it reads back. WebView2 ignores it.
pub const LAUNCHER_PORT_VAR: &str = "LTF_CDP_PORT";

/// The variable the Sona LTF launcher sets. A start marked by it keeps the switches of
/// [`DEBUG_SWITCHES`], and `verify_browser_process` does not close it for them.
///
/// Any process of the same user can set the variable, so it marks a start path rather than
/// authenticates the launcher. ADR-0082 states the exception.
pub const SONA_LTF_BRIDGE: &str = "SONA_LTF_BRIDGE";

/// WebView2 variables that replace the runtime folder, the release channel or the user data
/// folder, or that attach a script debugger.
pub const OVERRIDE_VARS: [&str; 7] = [
    "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
    "WEBVIEW2_USER_DATA_FOLDER",
    "WEBVIEW2_CHANNEL_SEARCH_KIND",
    "WEBVIEW2_RELEASE_CHANNELS",
    "WEBVIEW2_RELEASE_CHANNEL_PREFERENCE",
    "WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER",
    "WEBVIEW2_WAIT_FOR_SCRIPT_DEBUGGER",
];

/// Chromium switches that let another process drive, script or intercept the frontend.
///
/// Names without their leading dashes, lowercase, as Chromium compares them on Windows.
pub const DEBUG_SWITCHES: [&str; 28] = [
    // Chrome DevTools Protocol
    "remote-debugging-port",
    "remote-debugging-pipe",
    "remote-debugging-io-pipes",
    "remote-debugging-address",
    "remote-allow-origins",
    "auto-open-devtools-for-tabs",
    "silent-debugger-extension-api",
    "enable-automation",
    // Code loaded into the page
    "load-extension",
    "disable-extensions-except",
    "js-flags",
    "disable-web-security",
    // Another executable started in place of a browser child process
    "renderer-cmd-prefix",
    "utility-cmd-prefix",
    "gpu-launcher",
    "browser-subprocess-path",
    "no-sandbox",
    // Traffic interception
    "proxy-server",
    "proxy-pac-url",
    "proxy-bypass-list",
    "proxy-auto-detect",
    "host-resolver-rules",
    "host-rules",
    "ignore-certificate-errors",
    "ignore-certificate-errors-spki-list",
    "allow-insecure-localhost",
    "ssl-key-log-file",
    "log-net-log",
];

/// What this process inherited through the WebView2 variables, which are removed on the way.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Inherited {
    /// The switches of [`BROWSER_ARGS_VAR`] that are passed on to WebView2.
    pub kept: Vec<String>,
    /// The switches of [`BROWSER_ARGS_VAR`] in [`DEBUG_SWITCHES`], which are not.
    pub dropped: Vec<String>,
    /// Whether [`LAUNCHER_PORT_VAR`] was set.
    pub launcher_port: bool,
    /// The variables of [`OVERRIDE_VARS`] that were set.
    pub overrides: Vec<&'static str>,
    /// Whether [`SONA_LTF_BRIDGE`] was set.
    pub sona_ltf: bool,
}

impl Inherited {
    /// Splits the value `args` of [`BROWSER_ARGS_VAR`] into the kept and the dropped switches.
    ///
    /// If `sona_ltf` is set, the switches in [`DEBUG_SWITCHES`] stay in `kept`, because the Sona
    /// LTF launcher reads the debugging port.
    pub fn new(
        args: &str,
        launcher_port: bool,
        overrides: Vec<&'static str>,
        sona_ltf: bool,
    ) -> Self {
        let Filtered { kept, mut dropped } = filter(args);
        let kept = if sona_ltf {
            kept.into_iter().chain(dropped.drain(..)).collect()
        } else {
            kept
        };

        Self {
            kept,
            dropped,
            launcher_port,
            overrides,
            sona_ltf,
        }
    }

    /// Returns `true` if a launcher tried to open a debugging channel into the frontend.
    pub fn is_debugging(&self) -> bool {
        self.launcher_port || !self.dropped.is_empty()
    }

    /// Returns `true` if no variable was set.
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
            && self.dropped.is_empty()
            && !self.launcher_port
            && self.overrides.is_empty()
            && !self.sona_ltf
    }
}

/// Removes the WebView2 variables and [`SONA_LTF_BRIDGE`] from this process and returns what
/// they held.
///
/// Runs before anything creates a webview, so WebView2 never reads the variables, and children
/// of this process do not inherit the marker.
pub fn take_inherited() -> Inherited {
    let args = std::env::var_os(BROWSER_ARGS_VAR);
    let launcher_port = std::env::var_os(LAUNCHER_PORT_VAR).is_some();
    let sona_ltf = std::env::var_os(SONA_LTF_BRIDGE).is_some();
    let overrides: Vec<&'static str> = OVERRIDE_VARS
        .into_iter()
        .filter(|name| std::env::var_os(name).is_some())
        .collect();

    for name in [BROWSER_ARGS_VAR, LAUNCHER_PORT_VAR, SONA_LTF_BRIDGE]
        .into_iter()
        .chain(OVERRIDE_VARS)
    {
        std::env::remove_var(name);
    }

    let args = args
        .as_deref()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default();
    Inherited::new(&args, launcher_port, overrides, sona_ltf)
}

/// The switches of a browser argument string, split by [`filter`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Filtered {
    /// The switches that are not in [`DEBUG_SWITCHES`], in order.
    pub kept: Vec<String>,
    /// The switches that are, in order.
    pub dropped: Vec<String>,
}

/// Splits the browser arguments `args` into the switches WebView2 may see and the debugging ones.
pub fn filter(args: &str) -> Filtered {
    let mut filtered = Filtered::default();
    for argument in arguments(args) {
        if is_debug_switch(&argument.value) {
            filtered.dropped.push(argument.raw);
        } else {
            filtered.kept.push(argument.raw);
        }
    }
    filtered
}

/// Returns `switches` cut before `=`, for a log. A value can hold a path or a proxy password.
pub fn without_values(switches: &[String]) -> Vec<&str> {
    switches
        .iter()
        .map(|switch| without_value(switch))
        .collect()
}

/// Returns `switch` cut before `=`.
fn without_value(switch: &str) -> &str {
    switch.split('=').next().unwrap_or(switch)
}

/// Returns the first debugging switch on `command_line`, or `None` if it has none.
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "only Windows reads the browser process")
)]
pub fn debug_switch_in(command_line: &str) -> Option<String> {
    arguments(command_line)
        .into_iter()
        .map(|argument| argument.value)
        .find(|value| is_debug_switch(value))
}

/// Returns the debugging switch on `command_line` that closes the manager, or `None` if the
/// browser process may keep it.
///
/// A Sona LTF start keeps its debugging port. Every other start is refused, because the check
/// does not know what set the switch.
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "only Windows reads the browser process")
)]
fn refused_debug_switch(command_line: &str, sona_ltf: bool) -> Option<String> {
    if sona_ltf {
        return None;
    }
    debug_switch_in(command_line)
}

/// The arguments the main window passes WebView2: `configured`, from `tauri.conf.json`, then
/// `kept`. Returns `None` if there are none, so wry keeps its own defaults.
pub fn browser_args(configured: Option<&str>, kept: &[String]) -> Option<String> {
    let mut args: Vec<&str> = configured.into_iter().collect();
    args.extend(kept.iter().map(String::as_str));
    (!args.is_empty()).then(|| args.join(" "))
}

/// Returns `true` if `argument` is a switch in [`DEBUG_SWITCHES`], whatever its value.
fn is_debug_switch(argument: &str) -> bool {
    let Some(name) = switch_name(argument) else {
        return false;
    };
    DEBUG_SWITCHES
        .iter()
        .any(|switch| name.eq_ignore_ascii_case(switch))
}

/// Returns the name of the switch `argument`, without its prefix and value. Returns `None` for
/// a plain argument.
///
/// Chromium on Windows takes `--name`, `-name` and `/name`.
fn switch_name(argument: &str) -> Option<&str> {
    let name = argument
        .strip_prefix("--")
        .or_else(|| argument.strip_prefix('-'))
        .or_else(|| argument.strip_prefix('/'))?;
    let name = name.split('=').next().unwrap_or(name);
    (!name.is_empty()).then_some(name)
}

/// One argument of a browser command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Argument {
    /// The argument as the command line spells it, with its quotes and backslashes.
    raw: String,
    /// The argument as `CommandLineToArgvW` returns it to Chromium.
    value: String,
}

/// Splits `line` into arguments by the rules of `CommandLineToArgvW`.
///
/// Chromium compares a switch name after that function removed the quotes, so
/// `--remote"-debugging-port"=1` is a debugging switch. A switch is matched on `value` and
/// passed on as `raw`.
fn arguments(line: &str) -> Vec<Argument> {
    let chars: Vec<char> = line.chars().collect();
    let mut arguments = Vec::new();
    let mut raw = String::new();
    let mut value = String::new();
    let mut started = false;
    let mut backslashes = 0;
    // Consecutive quotes, counted from the one that opened a quoted part. 1 inside quotes.
    let mut quotes = 0;

    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;

        if (c == ' ' || c == '\t') && quotes == 0 {
            if started {
                arguments.push(Argument {
                    raw: std::mem::take(&mut raw),
                    value: std::mem::take(&mut value),
                });
            }
            started = false;
            backslashes = 0;
            continue;
        }

        started = true;
        raw.push(c);
        match c {
            '\\' => {
                value.push(c);
                backslashes += 1;
            }
            '"' => {
                // 2n backslashes and a quote are n backslashes and a delimiter. 2n+1 are n
                // backslashes and a literal quote.
                value.truncate(value.len() - (backslashes - backslashes / 2));
                if backslashes % 2 == 0 {
                    quotes += 1;
                } else {
                    value.push('"');
                }
                backslashes = 0;

                // Every third consecutive quote is a literal one.
                while chars.get(index) == Some(&'"') {
                    index += 1;
                    raw.push('"');
                    quotes += 1;
                    if quotes == 3 {
                        value.push('"');
                        quotes = 0;
                    }
                }
                if quotes == 2 {
                    quotes = 0;
                }
            }
            _ => {
                value.push(c);
                backslashes = 0;
            }
        }
    }

    if started {
        arguments.push(Argument { raw, value });
    }
    arguments
}

#[cfg(windows)]
mod browser_process;
#[cfg(windows)]
pub use browser_process::verify_browser_process;

#[cfg(test)]
mod tests;
