//! Child-process helpers for the agent drivers: PATH lookup, spawning without a console window,
//! killing the whole process tree, and keeping the tail of stderr for error messages.
//!
//! `renderly_core::proc::command` is crate-private, so [`command`] mirrors its behaviour: on
//! Windows the child gets `CREATE_NO_WINDOW`, so a GUI-subsystem app never flashes a console.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};

/// `Command::new` plus `CREATE_NO_WINDOW` on Windows (mirrors `renderly_core::proc::command`).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new(program);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        Command::new(program)
    }
}

/// Find `name` on `PATH`. On Windows it tries `.exe`, `.cmd`, `.bat`, then the bare name.
/// Never scans directories outside `PATH`.
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path_var) {
        for suffix in suffixes {
            let candidate = dir.join(format!("{name}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Spawn `program args…` with piped stdio in `cwd`. `.cmd` / `.bat` shims (npm installs on
/// Windows) are run through `cmd /C`, because Rust refuses to pass arbitrary arguments to batch
/// files directly.
pub fn spawn(program: &Path, args: &[String], cwd: &Path) -> std::io::Result<Child> {
    let is_batch = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut cmd = if is_batch {
        let mut c = command("cmd");
        c.arg("/C").arg(program);
        c
    } else {
        command(program)
    };
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
}

/// Run a short status command to completion. Returns `(exited_successfully, first line of
/// stdout)`, or `None` if it could not be started. Batch shims go through `cmd /C`.
pub fn run_capture(program: &Path, args: &[&str]) -> Option<(bool, String)> {
    let is_batch = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut cmd = if is_batch {
        let mut c = command("cmd");
        c.arg("/C").arg(program);
        c
    } else {
        command(program)
    };
    let output = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Some((output.status.success(), text))
}

/// Keeps the last lines written to stderr, so an exit can be reported with context.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

const TAIL_LINES: usize = 20;

impl StderrTail {
    pub fn last_lines(&self) -> String {
        self.0
            .lock()
            .map(|q| q.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }
}

/// Drain a child's stderr on a background thread so the pipe never fills and blocks the child.
pub fn drain_stderr(stderr: ChildStderr) -> StderrTail {
    let tail = StderrTail::default();
    let sink = tail.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Ok(mut q) = sink.0.lock() {
                if q.len() == TAIL_LINES {
                    q.pop_front();
                }
                q.push_back(line);
            }
        }
    });
    tail
}

/// Kill a child and everything it started. On Windows this uses `taskkill /T` so Node-based
/// shims do not leave orphans behind; elsewhere it kills the direct child.
pub fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = command("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Read stdout line by line on the current thread, calling `on_line` for each.
pub fn read_lines(stdout: impl Read, mut on_line: impl FnMut(&str)) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        on_line(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_is_none() {
        assert!(resolve_program("renderly-definitely-not-a-real-cli").is_none());
    }

    #[test]
    fn stderr_tail_keeps_only_recent_lines() {
        let tail = StderrTail::default();
        {
            let mut q = tail.0.lock().unwrap();
            for i in 0..(TAIL_LINES + 5) {
                if q.len() == TAIL_LINES {
                    q.pop_front();
                }
                q.push_back(format!("line {i}"));
            }
        }
        let text = tail.last_lines();
        assert!(text.starts_with("line 5"));
        assert!(text.ends_with(&format!("line {}", TAIL_LINES + 4)));
    }
}
