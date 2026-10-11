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
