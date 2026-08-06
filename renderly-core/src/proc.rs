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
use std::process::Command;

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
