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

/// Runs a program that may start programs of its own (an extension running
/// Python that runs FFmpeg and Whisper), handing each line it writes, to stdout
/// or stderr, to `on_line`. When `cancel` is set the whole tree is ended, not
/// just the program: on Windows a grandchild left running keeps the files it
/// opened. Programs still running when it ends are ended too. None when cancelled.
pub fn tree_or_cancel(cmd: &mut Command, cancel: &AtomicBool, on_line: impl Fn(&str) + Send + Sync + 'static) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let tree = Tree::of(&child);
    let on_line = std::sync::Arc::new(on_line);
    let readers: Vec<_> = [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)]
        .into_iter()
        .flatten()
        .map(|pipe| {
            let on_line = on_line.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(pipe);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                    on_line(String::from_utf8_lossy(&line).trim_end());
                    line.clear();
                }
            })
        })
        .collect();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if cancel.load(Ordering::Relaxed) {
            tree.kill(&mut child);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // What the program started and left behind still holds the pipes open.
    tree.kill(&mut child);
    for r in readers {
        let _ = r.join();
    }
    Ok(status)
}

/// A program and everything it starts: a Windows job object that ends them all
/// at once, and also when OrangeLink itself closes (the handle closes then).
#[cfg(windows)]
struct Tree(Option<windows::Win32::Foundation::HANDLE>);

#[cfg(windows)]
impl Tree {
    #[allow(unsafe_code)]
    fn of(child: &std::process::Child) -> Tree {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
        };
        // SAFETY: the job handle is ours until `Drop` closes it; the info struct
        // lives on the stack for the call, with its true size; the child's handle
        // stays open while `child` is borrowed.
        unsafe {
            let Ok(job) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else { return Tree(None) };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&info) as u32);
            if set.is_err() || AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
                let _ = CloseHandle(job);
                return Tree(None);
            }
            Tree(Some(job))
        }
    }

    #[allow(unsafe_code)]
    fn kill(&self, child: &mut std::process::Child) {
        match self.0 {
            // SAFETY: the handle is open until `Drop`.
            Some(job) => unsafe {
                let _ = windows::Win32::System::JobObjects::TerminateJobObject(job, 1);
            },
            None => {
                let _ = child.kill();
            }
        }
    }
}

#[cfg(windows)]
impl Drop for Tree {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            // SAFETY: closed once, here.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(job);
            }
        }
    }
}

/// Elsewhere only the program itself is ended (OrangeLink ships for Windows).
#[cfg(not(windows))]
struct Tree;

#[cfg(not(windows))]
impl Tree {
    fn of(_: &std::process::Child) -> Tree {
        Tree
    }

    fn kill(&self, child: &mut std::process::Child) {
        let _ = child.kill();
    }
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

    #[test]
    fn cancelling_ends_the_programs_a_program_started() {
        // `cmd` starts `ping`, which writes into the file through the handle it
        // inherited. Ending only `cmd` left ping running, holding the file, so it
        // could not be deleted.
        let dir = std::env::temp_dir().join(format!("orangelink-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("out.txt");
        #[cfg(windows)]
        let mut cmd = hidden("cmd");
        // Raw: cmd.exe does not read the quoting other programs get.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::raw_arg(&mut cmd, format!("/c ping -n 60 127.0.0.1 > \"{}\"", file.display()));
        #[cfg(not(windows))]
        let mut cmd = hidden("sh");
        #[cfg(not(windows))]
        cmd.arg("-c").arg(format!("sleep 60 > '{}' 2>&1", file.display()));
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            flag.store(true, Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        assert!(tree_or_cancel(&mut cmd, &cancel, |_| {}).unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(20), "waited for the program to finish");
        // The handle goes as the process ends; give Windows a moment.
        let mut removed = false;
        for _ in 0..20 {
            if std::fs::remove_file(&file).is_ok() {
                removed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(removed, "a program started by the cancelled one still holds its file");
    }

    #[test]
    fn every_line_of_both_outputs_is_handed_on() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = lines.clone();
        #[cfg(windows)]
        let mut cmd = hidden("cmd");
        #[cfg(windows)]
        std::os::windows::process::CommandExt::raw_arg(&mut cmd, "/c echo one& echo two 1>&2& echo three");
        #[cfg(not(windows))]
        let mut cmd = hidden("sh");
        #[cfg(not(windows))]
        cmd.args(["-c", "echo one; echo two 1>&2; echo three"]);
        let status = tree_or_cancel(&mut cmd, &AtomicBool::new(false), move |l| crate::lock(&seen).push(l.trim().to_string())).unwrap().unwrap();
        assert!(status.success());
        let mut got = crate::lock(&lines).clone();
        got.sort();
        assert_eq!(got, ["one", "three", "two"]);
    }
}
