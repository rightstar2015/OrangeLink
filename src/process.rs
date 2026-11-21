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
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

/// Like `Command::output`, but the program is killed, and waited for, as soon as
/// `cancel` is set: on Windows a program left running keeps its files open.
/// None when cancelled.
pub fn output_or_cancel(cmd: &mut Command, cancel: &AtomicBool) -> std::io::Result<Option<Output>> {
    output_or_cancel_lines(cmd, cancel, |_| {})
}

/// `output_or_cancel`, also handing each line the program writes to stdout to
/// `on_line` as it comes (FFmpeg's `-progress pipe:1`).
pub fn output_or_cancel_lines(cmd: &mut Command, cancel: &AtomicBool, mut on_line: impl FnMut(&str) + Send + 'static) -> std::io::Result<Option<Output>> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    // Read both pipes on the side so a chatty program can't fill one and stall.
    let stdout = child.stdout.take().map(|p| {
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut buf = Vec::new();
            let mut reader = std::io::BufReader::new(p);
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                on_line(String::from_utf8_lossy(&line).trim_end());
                buf.append(&mut line);
            }
            buf
        })
    });
    let stderr = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut p, &mut buf);
            buf
        })
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let joined = |t: Option<std::thread::JoinHandle<Vec<u8>>>| t.and_then(|t| t.join().ok()).unwrap_or_default();
    let (stdout, stderr) = (joined(stdout), joined(stderr));
    Ok(status.map(|status| Output { status, stdout, stderr }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_program_is_ended_not_left_running() {
        #[cfg(windows)]
        let mut cmd = hidden("ping");
        #[cfg(windows)]
        cmd.args(["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut cmd = hidden("sleep");
        #[cfg(not(windows))]
        cmd.arg("30");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        assert!(output_or_cancel(&mut cmd, &cancel).unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(10), "waited for the program to finish");

        let out = output_or_cancel(hidden("cmd").args(["/c", "echo done"]), &AtomicBool::new(false)).unwrap().unwrap();
        assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("done"));
    }

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
