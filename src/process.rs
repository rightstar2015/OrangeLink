//! Starting other programs. Every `Command` comes from here (clippy.toml
//! disallows `Command::new` elsewhere), so no call site has to remember the
//! Windows details on its own:
//!
//! - [`hidden`]: a tool OrangeLink drives (FFmpeg, yt-dlp, curl, tar, the
//!   updater). No console window, and stdin is null: the windowed build has no
//!   console handles to hand down, and FFmpeg waiting on stdin stalled frame
//!   capture there.
//! - [`visible`]: a program the user asked to see (an "open with" program,
//!   Explorer, winget in its own console, OrangeLink relaunched after an update).

use std::ffi::OsStr;
use std::process::{Command, Stdio};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A tool run in the background: no console window, stdin null. Set stdout and
/// stderr at the call site, as the tool's output is needed or not.
#[allow(clippy::disallowed_methods)] // the one place background tools are made
pub fn hidden(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// A program the user sees, started as Windows would start it.
#[allow(clippy::disallowed_methods)] // the one place visible programs are made
pub fn visible(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_tools_get_no_stdin() {
        // `cmd /c` with stdin null reads nothing and exits at once instead of
        // waiting for input that never comes.
        #[cfg(windows)]
        let out = hidden("cmd").args(["/c", "set /p x= & echo done"]).output().unwrap();
        #[cfg(not(windows))]
        let out = hidden("sh").args(["-c", "read x; echo done"]).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("done"));
    }
}
