# Renderly skills for Claude Code

These are Claude Code skills for driving Renderly through the `renderly-mcp` server. Each one is a
`SKILL.md` procedure that uses only tools and commands that exist today (see
[docs/mcp-agent-guide.md](../docs/mcp-agent-guide.md)).

| Skill | What it does |
|---|---|
| `remove-silences` | Cuts silent pauses out of a clip (detect silence, split, ripple delete). |
| `rough-cut-from-transcript` | Keeps the transcript segments you ask for and removes the rest. |
| `captions-pass` | Generates Whisper captions, restyles them, and checks a frame. |
| `fit-to-length` | Condenses the timeline to a target length using pauses, and speed only with agreement. |

Not included: word-level timing and beat detection. Renderly does not build them yet, so no skill
relies on them.

## Install

Claude Code loads skills from two places (see the Claude Code skills documentation at
<https://code.claude.com/docs/en/skills>):

- **Personal** (all your projects): `~/.claude/skills/<skill-name>/SKILL.md`
- **Project** (this repository only): `.claude/skills/<skill-name>/SKILL.md`

To install all four for yourself, copy each folder into your personal skills directory. On
Windows, `%USERPROFILE%\.claude\skills\`; on macOS or Linux, `~/.claude/skills/`:

```sh
mkdir -p ~/.claude/skills
cp -r skills/remove-silences skills/rough-cut-from-transcript skills/captions-pass skills/fit-to-length ~/.claude/skills/
```

To use them only inside a project, copy them to that project's `.claude/skills/` folder instead.

Claude Code watches these folders, so new skills appear in the current session. If you created
the top-level `skills` folder during a session, run `/reload-skills` once.

## Requirements

- The `renderly-mcp` server, registered with Claude Code under the name `renderly`. The skills
  call its tools by their bare names (`get_project`, `apply_command`, `detect_silence`, ...);
  Claude Code exposes them as `mcp__renderly__<tool>`. See the "Running the server" section of
  [docs/mcp-agent-guide.md](../docs/mcp-agent-guide.md).
- A project open in `renderly-mcp` (`open_project` or `new_project`). With the desktop app open on
  the same project, edits go through it and appear live.
- For `detect_silence`, `detect_scenes`, `get_audio_peaks` and `render_frame`: FFmpeg on the
  `PATH`. For `get_transcript` and `GenerateCaptions`: `whisper-cli` and a Whisper model
  (`RENDERLY_WHISPER_MODEL`).

## Editing rules every skill follows

Each skill follows the edit contract in [docs/command-api.md](../docs/command-api.md):

- It passes `expected_revision` from its last read, and then the `revision` from each write.
- It dry-runs the first call of each kind before committing.
- It deletes only with `allow_deletion`, listing exactly the ids a dry run reports.
- It never edits the project file directly.
