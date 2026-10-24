//! Magnetic tracks (schema v8): on a track with `magnetic: true`, clips stay contiguous from 0,
//! frame-snapped. Every command that changes which clips a track holds, their order or their
//! lengths ends by packing the track, so agents get the behaviour the timeline UI has and the
//! whole edit is one undo step (docs/command-api.md "Magnetic tracks").

use super::Command;
use crate::project::{Clip, Id, Project, Track};
use crate::time::Fps;

/// Pack `track`: clips in start order (stable), each starting where the previous one ends,
/// the first at 0. Starts are frame-snapped; the clip lengths are untouched.
pub(super) fn pack(track: &mut Track, fps: Fps) {
    track.clips.sort_by_key(Clip::start_us);
    let mut cursor = 0_i64;
    for clip in &mut track.clips {
        clip.set_start_us(cursor);
        let end = fps.snap_to_frame(cursor + clip.duration_us());
        // A clip never advances the cursor by less than a frame, so starts stay distinct.
        cursor = end.max(cursor + fps.frame_start(1).max(1));
    }
}

/// Pack the track `track_id` when it is magnetic.
pub(super) fn pack_if_magnetic(project: &mut Project, track_id: Id) {
    let fps = project.settings.fps;
    if let Some(track) = project.find_track_mut(track_id) {
        if track.magnetic {
            pack(track, fps);
        }
    }
}

/// Pack every magnetic track of the project (root timeline and sequences).
pub(super) fn pack_all(project: &mut Project) {
    let fps = project.settings.fps;
    for track in project.all_tracks_mut() {
        if track.magnetic {
            pack(track, fps);
        }
    }
}

/// Insert `clip` into the magnetic `track` where its centre falls among the clips already
/// there (each at its current position): it goes after every clip whose centre is at or before
/// its own. The track is then packed. Gaps never survive, so a start beyond the end appends.
pub(super) fn insert(track: &mut Track, clip: Clip, fps: Fps) {
    let centre = clip.start_us() + clip.duration_us() / 2;
    track.clips.sort_by_key(Clip::start_us);
    let at = track
        .clips
        .iter()
        .take_while(|c| c.start_us() + c.duration_us() / 2 <= centre)
        .count();
    track.clips.insert(at, clip);
    // Re-assign starts in list order: sorting by the old starts would undo the insertion.
    let mut cursor = 0_i64;
    for c in &mut track.clips {
        c.set_start_us(cursor);
        let end = fps.snap_to_frame(cursor + c.duration_us());
        cursor = end.max(cursor + fps.frame_start(1).max(1));
    }
}

/// The tracks a command may leave unpacked, which are packed afterwards when magnetic. `None`
/// means the command's reach is not named by its parameters (templates): every magnetic track
/// is packed.
pub(super) fn touched_tracks(command: &Command) -> Option<Vec<Id>> {
    Some(match command {
        Command::DeleteClip { track_id, .. }
        | Command::TrimClip { track_id, .. }
        | Command::SplitClip { track_id, .. }
        | Command::SetClipSpeed { track_id, .. }
        | Command::SetClipKeyframes { track_id, .. }
        | Command::AddAdjustmentLayer { track_id, .. }
        | Command::SetAdjustmentLayer { track_id, .. }
        | Command::AddStickerFromPack { track_id, .. }
        | Command::SetTextTiming { track_id, .. }
        | Command::UnnestClip { track_id, .. } => vec![*track_id],
        Command::NestClips { clips, .. } => clips.iter().map(|c| c.track_id).collect(),
        Command::ApplyTemplate { .. }
        | Command::ApplyTranscriptCuts { .. }
        | Command::RestoreTranscriptWords { .. } => return None,
        _ => Vec::new(),
    })
}
