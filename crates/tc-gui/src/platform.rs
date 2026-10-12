//! Shell integration: revealing files in the system file manager.

use std::io;
use std::path::Path;
use std::process::Command;

/// Opens the system file manager with `path` selected.
pub fn show_in_file_manager(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer parses its own command line; the path must be quoted after `/select,`.
        Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

/// Starts this program again with administrator rights (Windows shows the UAC prompt), passing
/// `path` as the folder to scan. Returns `Ok(false)` if the user declined the prompt; on
/// `Ok(true)` the caller should exit.
#[cfg(windows)]
pub fn relaunch_elevated(path: &str) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::ERROR_CANCELLED;
    use windows_sys::Win32::UI::Shell::{SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let exe = wide(std::env::current_exe()?.as_os_str());
    let verb = wide("runas".as_ref());
    let params = wide(quote_arg(path).as_ref());
    // SAFETY: zero is a valid bit pattern for this plain C struct (null pointers, no flags).
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: `info` is initialised and its strings are NUL-terminated and outlive the call.
    if unsafe { ShellExecuteExW(&mut info) } != 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Quotes one command-line argument the way `CommandLineToArgvW` (and Rust's `std::env::args`)
/// parse it: backslashes are literal except right before a quote, where they must be doubled.
#[cfg(any(windows, test))]
fn quote_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // The n backslashes already written become 2n, plus one escaping the quote.
                out.extend(std::iter::repeat_n('\\', backslashes + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes precede the closing quote, so they are doubled too.
    out.extend(std::iter::repeat_n('\\', backslashes));
    out.push('"');
    out
}

/// Formats a Unix timestamp as local date and time.
pub fn format_time(secs: i64) -> String {
    if secs == 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quoting_survives_trailing_backslashes_and_quotes() {
        assert_eq!(quote_arg(r"C:\"), r#""C:\\""#);
        assert_eq!(quote_arg(r"C:\Program Files"), r#""C:\Program Files""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(r"\\server\share\"), r#""\\server\share\\""#);
    }
}
