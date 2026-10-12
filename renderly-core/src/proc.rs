//! Subprocess spawning helper. Every external tool Renderly shells out to (ffmpeg,
//! ffprobe, whisper, piper, the optional segmentation CLI) must go through [`command`]
//! rather than `std::process::Command::new` directly.
//!
//! On Windows, a process spawned from a GUI-subsystem app (the desktop app builds with
//! `windows_subsystem = "windows"`) pops a console window unless `CREATE_NO_WINDOW` is
//! set — so a plain `Command::new("ffmpeg")` flashes a black terminal on every probe,
//! thumbnail, waveform, playback premix, and export. Routing all spawns through this
//! helper suppresses that flash while staying a no-op on every other platform.

use std::ffi::OsStr;
use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Mutex;

/// Held across every process creation. On Windows a child inherits every inheritable
/// handle open in the parent at the instant of `CreateProcess`, which includes the pipe
/// ends another thread has just created for *its* child. A long-lived ffmpeg then keeps a
/// sibling's stdout pipe open, the sibling's reader never sees EOF, and two threads waiting
/// on each other's children deadlock. Serialising the short create step (not the child's
/// lifetime) closes that window: by the time the lock is released the parent has dropped
/// its copies of the child's pipe ends.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Process-creation methods that hold [`SPAWN_LOCK`]. Use these instead of
/// `Command::{spawn, output, status}` everywhere in this crate.
pub(crate) trait SerialSpawn {
    fn spawn_serial(&mut self) -> io::Result<Child>;
    /// Like `Command::output`: stdin is null, stdout and stderr are captured.
    fn output_serial(&mut self) -> io::Result<Output>;
    fn status_serial(&mut self) -> io::Result<ExitStatus>;
}

impl SerialSpawn for Command {
    fn spawn_serial(&mut self) -> io::Result<Child> {
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        self.spawn()
    }

    fn output_serial(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.spawn_serial()?.wait_with_output()
    }

    fn status_serial(&mut self) -> io::Result<ExitStatus> {
        self.spawn_serial()?.wait()
    }
}

/// Construct a [`Command`] with the platform tweaks Renderly needs for its subprocesses.
/// Identical to `Command::new` except that on Windows it sets `CREATE_NO_WINDOW` so the
/// child never flashes a console window (see the module doc).
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // winbase.h CREATE_NO_WINDOW — avoids linking the `windows`/`winapi` crates for a
        // single constant.
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
