# In-app agent chat: design note

Status: **Phase 1 implemented (2026-10-15), not yet verified with a signed-in CLI.** Parity row: [parity-board.md](parity-board.md) "In-app agent chat" (⬜).

**Phase 1 implemented:** what works and what does not is listed in §11. The Claude Code path is
built on the documented headless flags; the Codex path is built from the published docs and is
**untested against the real `codex` binary**. The manual checks the owner must run are in
[qa-checklist.md](qa-checklist.md) ("Agent chat").
Date of research: 2026-10-15. Source-of-truth specs this builds on:
[mcp-agent-guide.md](mcp-agent-guide.md), [bridge-protocol.md](bridge-protocol.md),
[command-api.md](command-api.md), [architecture.md](architecture.md), [AGENTS.md](../AGENTS.md).

Goal: a chat panel in `renderly-app` through which the user drives an **external coding agent they
already have** (Claude Code, OpenAI Codex CLI, Gemini CLI, Google Antigravity, others) to edit the open
project. The agent edits only through `renderly-mcp` (command API plus perception tools), so edits land
in the open GUI and share the app's undo stack. Renderly bundles no model, proxies no API traffic, and
never holds the user's agent credentials.

---

## 0. Integrated design (current, 2026-10-16)

This section supersedes §1, §3 and §4 for the Claude Code path. Those sections are kept as the
history of how the chat was first built. The reason for the change: the owner found the Phase-1
agent "built on top" of the app. It was slow and spent tokens. Claude Code ran with its full
coding prompt, settings and built-in tools. Every edit crossed three processes. The panel showed
raw tool names and JSON.

Status: implemented on the worktree branch; covered by unit and fake-agent tests. Not yet run
against the real signed-in `claude` binary. See [qa-checklist.md](qa-checklist.md) "Agent chat,
integrated".

### 0.1 Launch (Claude Code)

One process per session, launched with:

| Flag | Why |
|---|---|
| `-p --input-format stream-json --output-format stream-json --verbose --include-partial-messages` | Headless streaming, as before. |
| `--mcp-config <file> --strict-mcp-config` | The file registers one `http` server, `renderly`, with `Authorization: Bearer <token>`. |
| `--setting-sources ""` and `--disable-slash-commands` | No user, project or local settings, hooks, plugins or skills. |
| working directory = the session's scratch folder | A project's own `CLAUDE.md` is never read. |
| `--system-prompt-file <brief>` | The Renderly brief **replaces** Claude Code's coding prompt. |
| `--tools Read,Grep,Glob` (default) or `--tools default` | Built-ins are read-only by default. The "Let the agent use its own tools" switch enables all of them, and each still asks. |
| `--permission-prompt-tool mcp__renderly__permission_prompt` | Answers prompts for built-in tools only. Renderly's own tools are pre-allowed and never prompt. |
| `--model <value>` `--effort <level>` | Only when the user chose one from the list Claude Code reported (§0.10). Shape-checked, then checked against the list when it is cached. With no choice, neither flag is passed and Claude Code's defaults apply. |
| `--session-id` / `--resume`, `--allowedTools` (last) | As before. `--allowedTools` lists the five Renderly tools and `Read,Grep,Glob`. |

Verified against `claude --help` (Claude Code 2.1.274): `--system-prompt-file`, `--setting-sources`,
`--tools`, `--model`, `--effort`, `--disable-slash-commands` and `--permission-prompt-tool` are all
listed. `--system-prompt-file` also appears in `claude.exe`. The argument builder has unit tests for
both switch states and for model and effort.

Not used, on purpose. `--bare` accepts only an API key and would break subscription sign-in.
`--safe-mode` says it disables MCP servers, and the docs do not say whether that includes
`--mcp-config`. **Not verified:** whether a user-level `~/.claude/CLAUDE.md` still loads under
`--setting-sources ""`. The qa checklist asks for this.

### 0.2 The tool server runs inside the app

There is no `renderly-mcp` subprocess and no loopback bridge hop for Claude Code. The app serves
MCP over **Streamable HTTP** on `127.0.0.1` at a random port (`agent_chat/mcp_http.rs`). It uses
rmcp 2.1 with the `transport-streamable-http-server` feature, in stateless JSON mode. Each session
gets a 256-bit token, and every request must present it in a `Bearer` header (constant-time check,
401 otherwise). The server starts with the session and stops with it. The only new dependencies are
hyper, hyper-util, http-body-util, bytes, tokio-util, tower-service, base64 and schemars. Ollama's
client (reqwest, already in the tree through renderly-core, now also a direct dependency of
renderly-app) was added later for §0.9. All are
MIT or Apache-2.0 and were already in the tree through Tauri or rmcp. Transport tests start the
server on an ephemeral port, check that unauthenticated and wrong-token requests get 401, list
tools and call one with the token, and check that stopping closes the port.

Codex uses the same endpoint (§0.8). Ollama does not use MCP at all: its loop calls the same tool
functions in-process (§0.9).

### 0.8 Codex

Codex runs as `codex app-server` (JSON-RPC over stdio, `jsonrpc` omitted). The Renderly endpoint
is passed as config overrides, and the token goes in the child's environment, never in argv:

```
codex -c mcp_servers.renderly.url="http://127.0.0.1:<port>/mcp" \
      -c mcp_servers.renderly.bearer_token_env_var="RENDERLY_AGENT_TOKEN" \
      -c project_doc_max_bytes=0 app-server
env: RENDERLY_AGENT_TOKEN=<session token>
```

| Key | Source | Verified |
|---|---|---|
| `-c key=value` (value parsed as TOML) | `codex --help` | Yes, against codex-cli 0.163.0 |
| `mcp_servers.<id>.url`, `mcp_servers.<id>.bearer_token_env_var` | config reference (learn.chatgpt.com/docs/config-file/config-reference) | Accepted at `thread/start` by the live CLI. That the tools are then listed and called is **not** verified (qa-checklist) |
| `project_doc_max_bytes=0` | config reference (documented as a size cap) | Not verified that 0 disables `AGENTS.md`. The scratch folder has no `AGENTS.md`, so it does not matter in practice |
| `thread/start` `baseInstructions` (replaces the built-in instructions), `sandbox` (`read-only` or `workspace-write`), `approvalPolicy: "on-request"`, `cwd` (scratch folder), `model` | `v2/ThreadStartParams.json` in the 0.163.0 schema | Schema, and accepted by the live CLI |
| `turn/start` `effort` | `v2/TurnStartParams.json` | Schema only; no turn was run |
| `model/list` after `initialize` | `v2/ModelListResponse.json` | Live (returned models, efforts, defaults) |
| Approval replies `{"decision":"accept"\|"decline"}` | `CommandExecutionRequestApprovalResponse.json`, `FileChangeRequestApprovalResponse.json` | Schema only |
| `mcpServer/elicitation/request` reply `{"action":"accept"}` for the `renderly` server | `McpServerElicitationRequestResponse.json` | Schema only. Whether Codex sends this for Renderly tool calls is **not** verified |

Not done, on purpose: dynamic tools. The schema has `item/tool/call` (`DynamicToolCallParams`), but
`thread/start` has no field that registers client tools, so there is no way to offer Renderly's
tools that way in this version. MCP over HTTP is the path.

The tools switch maps to the sandbox. With "Let the agent use its own tools" off, the sandbox is
`read-only` and command and file approvals are declined without a prompt. On, it is
`workspace-write` (in the scratch folder) and each approval asks. The session runs in the scratch
folder, not the project folder. Tool names are normalised, so a `renderly` MCP call shows as
`mcp__renderly__<tool>`, the same action lines as Claude Code.

### 0.9 Ollama (local, Renderly runs the loop)

`ollama.rs`. There is no CLI to run. Renderly talks to Ollama's HTTP API and runs the agent loop.
Sources and captured shapes are in the module header
(github.com/ollama/ollama `docs/api.md`, checked against a local server on 2026-10-16).

- **Endpoints.** `GET /api/tags` (installed models), `POST /api/show` per model (`capabilities`,
  `model_info["<arch>.context_length"]`), `POST /api/chat` with `stream: true`, `tools`,
  `options.num_ctx`, and `think` for thinking models. Address: the panel's Ollama address, else
  `OLLAMA_HOST` (a scheme-less value gets `http://`), else `http://127.0.0.1:11434`.
- **Model requirement.** Only models whose `/api/show` capabilities include `tools` are offered.
  `vision` adds the `look` tool. `thinking` maps effort to `think` (`low`/`medium`/`high`; with no
  effort chosen, `think: true`). A model without `thinking` offers no effort. Model without
  `tools` fails at start with a message naming it.
- **Context size.** `num_ctx = min(model window, 32768)`; 8192 when the model reports none. History
  is kept under about `num_ctx × 3` characters by dropping the oldest user turns. The brief and the
  newest turn are kept.
- **Tools.** The slim set (`tool_definitions`): typed `add_caption`, `trim_clip`, `split_clip`,
  `move_clip`, `delete_clip`, `set_clip_effects`, `set_transition`, `set_transform`, `add_track`,
  `import_media`; the generic `edit` with `commands: [object]`; `edit_help` (the command reference,
  generated from the `Command` schema, or one command's schema on request); `timeline`,
  `find_media`, `assets`; `look` only with vision. Typed calls are turned into `Command` objects
  and run through `run_edit`, so they are one undo step and show as `mcp__renderly__edit`.
  Measured: the slim set is **6,193 bytes** (with `look`); the Claude `edit` input schema is
  **29,258 bytes** (`cargo test -p renderly-app --lib slim_tool_schemas -- --nocapture`).
- **Loop.** Each round streams one reply. Text deltas go to the panel (`<think>…</think>` blocks are
  removed, since some models write them into `content` even with `think: false`). Tool calls run
  through the broker (same classes as MCP: perception allowed, edits allowed when auto-edits is on,
  file-writing commands asked). The results go back as `tool` messages. At most **16 tool rounds**
  per turn, then a message and `tool_round_limit`. Cancel aborts the task, which drops the HTTP
  stream.
- **Verified.** A real turn with `qwen3:4b` on the local server called `add_caption` through the
  in-process path and the caption was added (`cargo test -p renderly-app --test ollama_loop --
  --ignored`). The model also streamed a lot of stray text, so the answer quality is the model's.
  Fake-server tests cover the round trip, the cap, cancel, filtering, and server-down.

### 0.10 Model lists (no fixed lists)

Nothing in Renderly lists models or effort levels. `models.rs` holds the shared types and the
parsers. Each agent answers a usage-free request, and the answer is cached per app session.
The panel refreshes it from the settings popover.

| Agent | Request | Where the list is | Sends a prompt? |
|---|---|---|---|
| Claude Code | stream-json `control_request` `{subtype:"initialize"}` | `control_response.response.models[]`: `value` (the `--model` alias), `displayName`, `supportsEffort`, `supportedEffortLevels`. Absent `account` means not signed in | No |
| Codex | `initialize`, then `model/list` | `data[]`: `model`, `displayName`, `isDefault`, `defaultReasoningEffort`, `supportedReasoningEfforts[]` | No (no thread, no turn) |
| Ollama | `GET /api/tags`, then `POST /api/show` each | Models with `tools` only | No |

Verified live on 2026-10-16 (Claude Code 2.1.301, codex-cli 0.163.0, a local Ollama). A Claude
signed-out state was not tested, so the "no account" rule is from the captured shape, not from a
signed-out run.

A saved choice is kept per agent (`renderly.agent.launch.v2`). When the agent no longer offers it,
the agent's default is used and the panel says so. Effort follows the model: a level is kept only
while the model offers it. The start call checks the choice against the cached list, and Ollama
needs a model, so it takes the list's default.

Status messages: not installed, not signed in, not running ("Ollama is not running at … Start
Ollama, then refresh."), no models (with the `ollama pull` hint), and errors. Each comes from the
backend's own text.

### 0.3 Tools

| Tool | Purpose | Output |
|---|---|---|
| `edit { commands: [Command] }` | Applies one or more commands as **one undo step**, through the same `apply_commands` path the GUI uses. | One plain line per command, and `Created:` with short ids of anything made. |
| `timeline { sequence_id? }` | Compact outline: settings, tracks in z-order, clips with short ids, spans, source ranges, media names, speed, effects, transitions. | Text. |
| `look { time_secs?, max_width? }` | The composited frame, through the live frame renderer. | MCP image (PNG), width at most `max_width` (default 768). |
| `find_media { dir, recursive?, max? }` | Media files with size. Duration and size are probed for the first 20. | Text. |
| `assets {}` | Built-in effects with defaults, transitions, caption styles, loaded packs and plugins. | Text. |
| `permission_prompt` | Claude Code's permission tool for built-in tools. Answers from the session broker. | JSON `{behavior}`. |

The `edit` schema is the real `Command` enum, generated by schemars. renderly-core gets it from a
non-default `schema` feature, which only `renderly-app` enables. The default build and the wasm
build do not include it. The schema for `edit` is about 29 KB, most of it variant docs. That is
large for a per-session tool listing and is the first thing to trim if it costs too much.

**Short ids.** Any id can be given as its first 8 hex characters, under keys ending in `id`,
`_id` or `_ids`. A prefix that matches exactly one project id is replaced with the full id before
the command is parsed. An ambiguous prefix is an error. A prefix that matches nothing is left
unchanged, so the parser reports it.

### 0.4 Context pushed with every user turn

`compose_turn` in `agent_chat/mod.rs` puts a block before each message:

```
[Renderly context: live project state, not a message from the user]
playhead 0:03.5
selected clips: ab12cd34
<timeline outline>
[end context]
```

The outline is cut at 1800 characters. When it is cut, the block says to call `timeline` for the
rest. For a typical two-track edit with captions, the block is 662 characters (about 165 tokens by
chars/4).

### 0.5 Undo per turn

`History` (in `lib.rs`) has an undo group. `agent_send` opens it, and the first edit of the turn
pushes the pre-turn snapshot. Later edits in the same turn add nothing, so one undo reverts the
whole turn. The group closes when the turn finishes, so the user's own edits afterwards are separate
steps. Undo is offered only on the latest turn's action list. Known gap: a GUI edit made while the
agent is still working joins that turn's group.

### 0.6 Panel

- An `edit` result becomes **action lines** in the transcript, for example "Trimmed 'Gameplay 1'
  to 0:00.0-0:05.0". The raw tool calls sit under one **Details** disclosure per turn.
  `timeline`, `look`, `assets` and `find_media` never appear as cards.
- A **working** line names the current activity ("Reading the timeline…", "Applying the edit…").
- Clips the edit touched or created are outlined on the timeline for 2.4 s
  (`agentChatStore.highlightClipIds`, drawn by `timeline/renderer.ts`).
- Settings are a gear popover: model, effort, and the built-in tools switch. They are remembered
  per user in localStorage, and apply when a session starts.
- The context chip is the selection only. The playhead, outline and sequence reach the agent in
  the context block.

### 0.7 Gaps and not done

- Codex: not yet run for a full turn, and whether Codex lists and calls the `renderly` MCP tools is
  not verified (§0.8). The handshake and the model list were run live.
- Ollama: one real turn was run (qwen3:4b). The other tool-capable models are untested.
- Model lists for Claude Code and Codex come from the CLIs' own answers, which were checked live.
  A signed-out Claude has not been run.
- Nothing has been run against the real `claude` binary. The fake-agent and HTTP tests cover the
  transport, the argument list and the event flow.
- `look` sets the width, not the longest side, so a portrait frame at the default width is about
  768 x 1364 px.
- The playhead does not move to a change that is off-screen. Highlights only.
- The brief (`RENDERLY_BRIEF` in `driver.rs`) is about 400 words, counted with `wc -w`. The
  spec's limit is 800.

## 1. Summary (Phase 1, superseded for Claude Code by §0)

**Decision (owner, 2026-10-15): drive the agent CLIs the user has already installed and signed
into, the way [T3 Code](https://github.com/pingdotgg/t3code) does.** Renderly never performs a login,
never sees a token or API key, and never calls a model API. It launches the vendor's own,
unmodified CLI as a child process; the CLI uses whatever account the user signed it into
(subscription or API key). This supersedes the earlier "API key by default" proposal and closes
Q1–Q3 in §9.

- **Embedded in the main editor window**, not a separate window or modal: a docked, resizable
  panel beside the inspector (§4.1).
- **Per-agent drivers behind one interface** in `renderly-app/src-tauri` (new `agent_chat`
  module). Each driver speaks the CLI's own headless protocol:

  | Agent | Driver | User setup |
  |---|---|---|
  | Codex | `codex app-server` — JSON-RPC over stdio, one process per session (as T3 Code does) | install Codex CLI, `codex login` |
  | Claude Code | `claude -p --input-format stream-json --output-format stream-json --verbose`, with `--mcp-config` pointing at `renderly-mcp` and a Renderly permission bridge | install Claude Code, `claude auth login` (or `/login`) |
  | Gemini CLI | `gemini --acp` (Google's own CLI speaking ACP) | install Gemini CLI, sign in |
  | Others | ACP, when the agent's own CLI speaks it natively | per agent |

  Third-party wrappers that re-implement a vendor's login (for example the ACP adapter built on
  the Claude Agent SDK) are **not** used.
- **Renderly's MCP server is the agent's only way to edit.** Each session hands the CLI
  `renderly-mcp --project <open project>` as an MCP server, so edits flow through the command API
  and the live bridge into the app's shared undo stack (§3.3, §3.4).
- **Not in `renderly-core`** (AGENTS.md §0.2: the engine never depends on UI or process
  management), and not in the webview (no process-spawn capability in the page).
- **Antigravity**: only if its own CLI exposes a documented headless or ACP mode; the
  `agy_acp_server` download in the ACP registry is not used (Q4).

The research below (§2) is kept for reference. Where it discusses ACP adapters and SDK login
policy, the decision above takes precedence.

---

## 2. Research findings

### 2.1 Agent Client Protocol

| Item | Finding | Source |
|---|---|---|
| What it is | Open protocol "for connecting any editor to any agent", originated by Zed. Spec repo is **Apache-2.0** | [agentclientprotocol.com](https://agentclientprotocol.com), [spec repo](https://github.com/agentclientprotocol/agent-client-protocol) (repo licence: Apache-2.0, checked via GitHub API) |
| Transport | JSON-RPC 2.0 over **stdio**; the agent runs as a subprocess of the client | [protocol overview](https://agentclientprotocol.com/protocol/overview) |
| Protocol version | Integer major version; **current = `1`** | [initialization](https://agentclientprotocol.com/protocol/initialization), [libraries](https://agentclientprotocol.com/libraries/overview) |
| Handshake | Client `initialize` sends `protocolVersion`, `clientCapabilities` (`fs.readTextFile`, `fs.writeTextFile`, `terminal`), `clientInfo`. Agent replies with `agentCapabilities` (`loadSession`, `mcpCapabilities.http/sse`, `promptCapabilities.image/audio/embeddedContext`), `agentInfo`, `authMethods` | [initialization](https://agentclientprotocol.com/protocol/initialization) |
| Auth | Agent advertises `authMethods` in `initialize`; the client calls `authenticate` with one. Logout is optional | [overview](https://agentclientprotocol.com/protocol/overview) |
| Session | `session/new` (requires absolute `cwd`), `session/load` (only if `loadSession`; agent replays history as `session/update`) | [session setup](https://agentclientprotocol.com/protocol/session-setup) |
| MCP servers | Passed **in `session/new`** as `mcpServers`. Stdio transport is mandatory for agents: `name`, `command` (absolute path), `args`, optional `env` (`[{name,value}]`). HTTP and SSE only if advertised (SSE is deprecated) | [session setup](https://agentclientprotocol.com/protocol/session-setup) |
| Prompt | `session/prompt` with content blocks; response carries `stopReason`: `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled` | [prompt turn](https://agentclientprotocol.com/protocol/prompt-turn) |
| Streaming | `session/update` notifications: message chunks, `tool_call` / `tool_call_update`, `plan` | [prompt turn](https://agentclientprotocol.com/protocol/prompt-turn), [tool calls](https://agentclientprotocol.com/protocol/tool-calls) |
| Tool calls | `tool_call` needs `toolCallId` and `title`; `kind` is one of `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`, `other`; `status` is `pending`, `in_progress`, `completed`, `failed`. Content may be `content`, `diff` (`path`, `oldText`, `newText`), or `terminal` (`terminalId`); `locations` carry `path` and `line` | [tool calls](https://agentclientprotocol.com/protocol/tool-calls) |
| Permissions | Agent sends `session/request_permission` with `toolCall` and `options` (`optionId`, `name`, `kind`: `allow_once`, `allow_always`, `reject_once`, `reject_always`). Outcome is `selected` or `cancelled` | [tool calls](https://agentclientprotocol.com/protocol/tool-calls) |
| Cancel | Client sends `session/cancel` (notification). Client must answer pending permission requests with `cancelled`; agent must return `stopReason: cancelled` | [prompt turn](https://agentclientprotocol.com/protocol/prompt-turn) |
| Client methods | `fs/read_text_file`, `fs/write_text_file` (need the fs capability); `terminal/create`, `terminal/output`, `terminal/release`, `terminal/wait_for_exit`, `terminal/kill` (need `terminal`) | [overview](https://agentclientprotocol.com/protocol/overview) |

**Client libraries (all Apache-2.0):**

| Language | Package | Licence | Source |
|---|---|---|---|
| Rust | `agent-client-protocol` (crates.io) | Apache-2.0 (repo `agentclientprotocol/rust-sdk`); crate version **unverified**, crates.io was not reachable | [rust-sdk](https://github.com/agentclientprotocol/rust-sdk) |
| TypeScript | `@agentclientprotocol/sdk` **1.8.0** | Apache-2.0 | [typescript-sdk](https://github.com/agentclientprotocol/typescript-sdk), [npm](https://www.npmjs.com/package/@agentclientprotocol/sdk) |
| Python | `python-sdk` | not checked | [python-sdk](https://github.com/agentclientprotocol/python-sdk) |
| Kotlin / Java | `acp-kotlin`, `java-sdk` | not checked | [kotlin-sdk](https://github.com/agentclientprotocol/kotlin-sdk), [java-sdk](https://github.com/agentclientprotocol/java-sdk) |

All four candidates we would use are AGPL-compatible (Apache-2.0 is compatible with AGPL-3.0 distribution).

### 2.2 Which agents speak ACP, and how

The authoritative list is the **ACP registry** ([agentclientprotocol/registry](https://github.com/agentclientprotocol/registry), Apache-2.0). Each entry has an `agent.json` with the distribution command. Four entries matter here:

| Agent | Registry id / version | Launch (per registry) | Adapter / agent licence | Auth model | MCP support |
|---|---|---|---|---|---|
| **Claude** (Claude Agent) | `claude-acp` **0.90.0** (preview 0.90.1-preview.14) | `npx @agentclientprotocol/claude-agent-acp@0.90.0` | Adapter repo LICENSE is **Apache-2.0** (checked). The registry manifest says `"license": "proprietary"`. **Conflict, flagged.** Wraps `@anthropic-ai/claude-agent-sdk` 0.3.296 (Commercial Terms, see §2.3) | ACP `authMethods`. The adapter has a `--hide-claude-auth` flag that refuses claude.ai subscription billing ([src/hide-claude-auth.ts](https://github.com/agentclientprotocol/claude-agent-acp/blob/main/src/hide-claude-auth.ts)). Without it, subscription login is offered | Client-provided MCP servers (stdio, HTTP, per capability negotiation) |
| **Codex CLI** | `codex-acp` **2.2.2** (preview 2.2.3-preview.8) | `npx -y @agentclientprotocol/codex-acp@2.2.2` (wraps the local `codex` CLI; `CODEX_PATH` overrides the binary) | Apache-2.0 per registry and README ([repo](https://github.com/agentclientprotocol/codex-acp)). GitHub's licence detector reports "Other"; **unverified**, confirm the LICENSE file | ChatGPT login offered as an ACP auth method (subscription); or `CODEX_API_KEY` / `OPENAI_API_KEY` (API billing). `NO_BROWSER=1` hides the login method | stdio and HTTP MCP servers from the client, `/mcp` command |
| **Gemini CLI** | `gemini` **0.64.0** | `npx @google/gemini-cli@0.64.0 --acp` | Apache-2.0 ([repo](https://github.com/google-gemini/gemini-cli)). Its `cli-reference.md` still documents `--experimental-acp`; the registry uses `--acp`. Check `gemini --help` in the installed version | "Sign in with Google" (free tier), Gemini API key, or Vertex AI | Not verified for the ACP path |
| **Google Antigravity** | `antigravity-acp` **1.4.0** | Prebuilt binary downloaded from `dl.google.com`: `./agy_acp_server.exe` on Windows (`--uid=` arg on Linux) | **Proprietary** (`antigravity.google/terms`). Not redistributable. Binary is Google's; Renderly would only tell the user where to get it | Google sign-in on first launch (community reports; unverified against Google docs) | Not verified |

Other registry entries, names only (licences and launch commands **not checked**): `opencode`, `goose`, `qwen-code`, `kimi`, `cursor`, `github-copilot`, `github-copilot-cli`, `junie`, `mistral-vibe`, `cline`, `pi-acp`, `poolside`, `qoder`, `fast-agent`, `factory-droid`, `crow-cli`, `kilo`, `amp-acp`, `devin`, `grok-build`, `auggie`, `vtcode`, and others. The [ACP agents page](https://agentclientprotocol.com/overview/agents) lists Claude Agent, Codex CLI, Gemini CLI, OpenCode and Poolside as registry-backed.

**Antigravity note.** Google's own ACP wrapper is in the registry, but the ACP site's agent list does not name Antigravity. Community bridges (`agy-acp-bridge`, `agy-agent-acp`) drive `agy` via an undocumented `--output-format stream-json` flag. Do not depend on those. One community project quotes Google's terms as prohibiting third-party access via OAuth (see §2.3).

**Do not use** `zed-industries/codex-acp` or `cola-io/codex-acp`. They are older or forked; the maintained entry is `agentclientprotocol/codex-acp`.

### 2.3 Non-ACP fallback: vendor SDKs and subscription terms

The question here is not "can we call it" but "can the user's subscription be used".

| Vendor | SDK | Licence / terms | Auth it accepts | Reuses CLI subscription login? |
|---|---|---|---|---|
| Anthropic | Claude Agent SDK (TS/Python) | Governed by **Commercial Terms of Service**, not OSS. Must not be bundled | `ANTHROPIC_API_KEY`, Bedrock, Vertex | **No, for third-party products.** "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK." ([Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview)) |
| Anthropic | Claude Code CLI itself | Consumer/Commercial terms | OAuth (subscription) or API key | **Yes, for the end user's own use.** "Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription" ([legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)) |
| OpenAI | Codex TypeScript SDK | Licence not stated on the page | Runs the local `codex` CLI as a child process; injects `CODEX_API_KEY`. ChatGPT-login behaviour **unverified** ([sdk README](https://github.com/openai/codex/blob/main/sdk/typescript/README.md)) | Unverified |
| OpenAI | Codex CLI sign-in | ChatGPT sign-in is "for subscription access"; API-key sign-in is "for usage-based access" ([Codex auth](https://learn.chatgpt.com/docs/auth)) | Either | Unverified for third-party clients. The page covers CLI, desktop app and IDE extension only |
| Google | Gemini CLI OAuth | Apache-2.0 code, but Google's terms say third-party tools using the CLI's OAuth login "is a violation of applicable terms" and "may be grounds for suspension or termination" ([Gemini CLI ToS page](https://www.geminicli.com/docs/resources/tos-privacy)) | Google sign-in (free tier), Gemini API key, Vertex | **No, for third-party tools.** Use a Gemini API key |
| Google | Agent Development Kit (ADK) | Apache-2.0 ([adk-python](https://github.com/google/adk-python)) | Model-agnostic, "optimized for Gemini"; auth details not on the README; **unverified** | Not a subscription path |

**Conclusion for the design.** The only sanctioned way to use a *subscription* from a third-party app is the user running the vendor's own unmodified CLI themselves. Renderly can host that CLI through ACP, but a Renderly-branded integration that brokers or reuses a login is exactly what the vendors prohibit. That is why the MVP defaults to API keys and leaves subscription login as an owner decision (§9).

---

## 3. Architecture (Phase 1; the Claude Code path is superseded by §0.1–§0.2, Codex unchanged)

> **Superseded for Claude Code.** Edits no longer go through a `renderly-mcp` subprocess and the
> loopback bridge. The app serves the tools itself (§0.2). §3.3 and §3.4 describe the Phase-1
> path that Codex still uses. The rest of this section is kept as history.

```
 +-------------------------- renderly-app (Tauri 2) ---------------------------+
 |  webview (React)                       Rust backend (src-tauri)             |
 |  +--------------------+   Tauri cmds   +-----------------------------------+ |
 |  | Agent chat panel   |<-------------->| agent_chat/ (new)                 | |
 |  | - transcript       |  events:       |  - agent registry + detection     | |
 |  | - tool-call cards  |  agent:update  |  - ACP client (Rust SDK)          | |
 |  | - permission modal |  agent:perm    |  - child process (stdio)          | |
 |  +--------------------+               |  - permission gate                 | |
 |                                        +----------------|------------------+ |
 |                                                         | stdio JSON-RPC    |
 |                                        +----------------v------------------+ |
 |                                        | agent subprocess (user's install) | |
 |                                        |  claude / codex / gemini / agy    | |
 |                                        +----------------|------------------+ |
 |                                                         | spawns (MCP,      |
 |                                                         |  stdio, from      |
 |                                                         |  session/new)     |
 |                                        +----------------v------------------+ |
 |                                        | renderly-mcp (stdio)              | |
 |                                        +----------------|------------------+ |
 |                                                         | loopback WS,     |
 |                                                         | token auth       |
 |                                        +----------------v------------------+ |
 |                                        | bridge.rs -> apply_command, undo  | |
 |                                        | shared undo stack, project:changed| |
 |                                        +-----------------------------------+ |
 +----------------------------------------------------------------------------+
```

### 3.1 Where the ACP client lives: Rust backend (recommended)

| Option | For | Against |
|---|---|---|
| **A. Rust in `src-tauri` (recommended)** | Process spawn and stdio pumping sit next to the existing bridge. No new webview capability for process execution. Keeps credentials and child-process handling out of the page. Rust SDK exists (§2.1). Lifecycle (kill on app close) is under our control | Need to write the Tauri event bridge for streamed updates. Rust SDK version and API unverified |
| B. TypeScript in the webview with `@agentclientprotocol/sdk` | Fastest UI iteration; the SDK is mature | Webview cannot spawn processes without the Tauri shell plugin, which widens the attack surface for any XSS in the chat UI. Webview CSP and process supervision are harder |
| C. Node sidecar running the TS SDK | Reuses the TS SDK and works with `npx` adapters | Ships a Node runtime or requires one; another process to supervise; still needs the Tauri side |

Rust in `src-tauri` is recommended. It matches AGENTS.md rule 2: the engine (`renderly-core`) gets **no** new code, and the agent chat is a UI-layer dispatcher. The chat only ever causes edits through `renderly-mcp`, which goes through the existing command API (rule 1).

### 3.2 Windows specifics

- `npx` on Windows is `npx.cmd`. Spawning it without a shell fails. Launch through `cmd /c npx ...` or resolve the Node bin path. Pick one and test it (unverified until built).
- Use `%LOCALAPPDATA%` discovery paths consistently with `bridge.rs`.
- Agents installed by vendor installers are found on `PATH` (`where claude`, `where codex`, `where gemini`). Detection is a `PATH` lookup, never a directory scan.

### 3.3 How the agent gets Renderly's MCP server

In `session/new`, Renderly passes:

```jsonc
{
  "cwd": "C:/Users/me/Videos/my-project",        // absolute, = project folder
  "mcpServers": [{
    "name": "renderly",
    "command": "C:/Program Files/Renderly/renderly-mcp.exe",   // absolute
    "args": ["--project", "C:/Users/me/Videos/my-project/edit.renderly.json"],   // see note
    "env": [{ "name": "RENDERLY_BRIDGE_DISCOVERY", "value": "" }]   // unset in real use
  }]
}
```

Notes:

- **The MCP server refuses to act without an open project.** `renderly-mcp/src/server.rs` returns `no project open; call open_project or new_project first`, and the agent guide says the bridge is used only when the project matches the app's open project. A bare `renderly-mcp` will therefore fail every edit until the agent calls `open_project`.
- Two bootstrap options:
  1. **Prompt the agent** to call `open_project` with the path from `get_editor_status`. Simple, but depends on model compliance, and the first turn can fail silently.
  2. **Add a pinned-project startup argument** (`--project <path>`) to `renderly-mcp`, which opens that file at startup. It keeps the safety rule (never act on whatever is on screen): the server opens only the path the app passed, and the bridge still checks `project_path` against the discovery file. **Recommended**, but it is a change to `renderly-mcp`, so it needs owner sign-off (Q5).
- `RENDERLY_BRIDGE_DISCOVERY` is for tests and sandboxes only (mcp-agent-guide). Never set it in the chat path.

### 3.4 How edits reach the GUI with shared undo

No new edit path. The chain is:

1. Agent calls `apply_command` on `renderly-mcp`.
2. `renderly-mcp` finds the live bridge via `bridge.json` and calls `apply_command` over the loopback WebSocket (bridge-protocol).
3. `bridge.rs` applies the command to the app's project, pushes to the app's history stack (shared undo), and emits `project:changed` so the timeline refetches.
4. The chat panel shows the same change through the tool-call card (below).

So Ctrl+Z in the app undoes an agent edit, and the chat undo button calls the bridge's `undo`. Per-turn undo grouping is an open question (Q6).

### 3.5 Session lifecycle

- One agent subprocess per open project, started on first message, kept alive for the session.
- `session/new` per chat thread. `session/load` only if the agent advertises `loadSession`.
- Closing the project or app: send `session/cancel`, wait briefly, then kill the child process tree (Windows: `taskkill /T`).
- A crashed agent shows an error card and a "restart agent" button. Renderly never restarts it silently.

---

## 4. UX (Phase 1 panel; superseded by §0.6)

> **Superseded.** The panel now shows plain action lines with one Undo per turn, and keeps raw
> tool calls under Details (§0.6). The Phase-1 tool cards and their permission model are below as
> history. Auto-allow of Renderly edits is no longer a switch: Renderly's own tools never prompt.

### 4.1 Placement: embedded in the main window

- The chat is part of the editor layout, always one click away: a **docked panel in the right
  column**, sharing it with the inspector through a tab strip ("Inspector" / "Agent"), and able to
  split the column vertically when both are wanted. Resizable; width persists per user.
- Toggled from the top bar and with a shortcut (`Ctrl+L`), which focuses the prompt box.
- Never covers the preview canvas or the timeline; the preview's `#preview-host` keeps its layout.
- The prompt box carries context chips (playhead time, selected clips, open sequence) that the
  user can remove before sending.
- Empty state: agent picker listing detected CLIs (§4.5), or "Connect your agent" instructions.

### 4.2 Message model

| Element | Shown as |
|---|---|
| User prompt | Plain bubble. Attach the playhead time and selection automatically (opt-out) |
| Agent text | Streamed chunks, Markdown, no HTML |
| Plan (`plan` update) | Collapsible checklist with status icons |
| Tool call (`tool_call`) | Card: title (e.g. `renderly.apply_command`), kind icon, status, collapsible JSON input and output |
| Renderly edit card | For `apply_command`: a human summary of the command ("Trim clip V1/3 to 2.0 s"), the RFC-6902 patch count, and an **Undo** button |
| Perception result | Thumbnail for `render_frame`, waveform/transcript snippet for `get_transcript` / `get_audio_peaks`. Full data collapsed |
| Terminal output | Only if the agent uses terminal capability (MVP: disabled) |
| Stop reason | Footer on each turn; `cancelled` and `refusal` get their own message |

### 4.3 Permissions

ACP only tells us *that* a tool needs approval (`session/request_permission`). It does not classify risk, so Renderly maps tools itself by name and `kind`:

| Tool class | Examples | Default |
|---|---|---|
| Perception (read-only) | `get_project`, `render_frame`, `get_transcript`, `detect_silence`, `detect_scenes`, `get_audio_peaks`, `probe_media`, `list_models` | Allow (no prompt) |
| Edits (undoable via shared undo) | `apply_command`, `apply_commands`, `NestClips`, `GenerateCaptions`, `GenerateVoiceover` | Allow, with an "Allow edits" toggle that is on by default for the session, visible in the header |
| File writes outside the project | `export`, `export_otio`, `import_otio` (writes a new project), `GenerateVoiceover` `output_path` | **Ask** each time |
| Export / render to disk | `export` | **Ask** each time |
| Model downloads | `download_model` | **Ask**, and the tool itself refuses without `user_consented` |
| Agent-native tools (shell, file edit, web) | depends on agent | **Ask**, and show the exact command or path |
| Anything not classified | new tools | **Ask** |

Rules:

- Options come from the agent (`allow_once`, `allow_always`, `reject_once`, `reject_always`). Renderly shows them, and `allow_always` is scoped to the session only, never persisted.
- A permission prompt never auto-approves on timeout. It stays pending and blocks the turn.
- Cancel answers every pending permission with `cancelled` (protocol requirement).

### 4.4 Other controls

- **Cancel** button: sends `session/cancel`. The turn ends with `cancelled`; any edits already applied remain, and the undo button covers them.
- **Agent picker**: list of detected agents with version, auth state, and a "how to sign in" link. Switching agent starts a new thread (agents do not share sessions).
- **Stop everything**: kills the subprocess tree.

### 4.5 First-run "connect your agent"

1. Detect installed agents (`PATH` lookup for `claude`, `codex`, `gemini`, `agy`; `node`/`npx` for the npx-distributed adapters). Show what is missing, with the install command from the registry manifest.
2. For the chosen agent, show the auth options that Renderly permits (§5.2). API-key entry is handled by the **vendor's** own CLI or environment variable, not by a Renderly form. Renderly does not store keys.
3. Run `initialize` and check `authMethods`. If the agent needs sign-in, show "Open a terminal and run `<agent login command>`" (command taken from the agent's own docs, not guessed) and a "Check again" button.
4. Confirm once: "This agent runs with your user account and can read and write files on this computer. Renderly will forward its permission requests to you." User acknowledges; stored locally.

---

## 5. Safety and privacy

### 5.1 Threat model

- **The agent runs with the user's privileges.** ACP client capabilities (`fs`, `terminal`) only govern what the agent *asks the client to do*. An agent's own native shell, file and network tools run regardless. Renderly therefore cannot sandbox the agent through ACP alone.
- **Mitigations we control:**
  - Set `cwd` to the project folder.
  - Advertise `fs` and `terminal` as **not supported** in `initialize` (MVP), so the agent cannot use Renderly as a file/terminal proxy at all.
  - Forward every agent-native permission request to the user (§4.3). Do not pass a permissive agent mode such as "bypass permissions" from Renderly. If the vendor's permission mode is configurable, Renderly sets the strictest mode it supports.
  - The MCP server is the only Renderly-provided tool surface. It exposes the same command set as the CLI, nothing more.
- **Prompt injection.** Transcripts, file names, captions and media metadata reach the agent through perception results. Treat them as data. Renderly never auto-approves a non-edit action because a tool result asked for it.
- **Bridge token.** `bridge.json` holds the loopback token and is readable by any process running as the user, including the agent. This is the same trust level as the user; document it, do not treat the bridge as a security boundary against the local user.

### 5.2 Credentials

- Renderly stores **no** vendor credentials. Login state stays with the vendor's CLI (`~/.claude`, `~/.codex/auth.json`, the Gemini CLI store, the Antigravity store).
- Renderly never reads, copies, or displays those files, and never asks for a key in its UI. It
  only asks the CLI whether it is signed in (its own status command or protocol call) and, if not,
  tells the user which command to run in a terminal (`claude auth login`, `codex login`, …).
- Whatever account the user signed the CLI into — subscription or API key — is what the session
  uses. That is the user running the vendor's own tool; Renderly is the screen it reports to.

### 5.3 Data leaving the machine

- Everything the agent sees (prompts, project JSON, transcripts, rendered frames) goes to **the vendor's model service through the user's own agent**. Renderly is not a party to that request and proxies nothing.
- The chat must say so in the first-run step and in the header ("Agent: Claude Code. Messages and project data are sent to Anthropic under your account.").
- Renderly sends no telemetry about chat content. No analytics on prompts.

### 5.4 Perception size limits

- `render_frame` returns base64 PNG in JSON (mcp-agent-guide, bridge-protocol). Proposed caps, to be validated: long edge ≤ 1280 px for frames sent to the agent, at most 4 images per turn, and total image bytes per turn ≤ 8 MB. Unverified against vendor image limits.
- Transcripts and peaks are truncated to a documented maximum with an explicit "truncated" marker.

### 5.5 Licensing

| Component | Licence | Use | Verdict |
|---|---|---|---|
| `agent-client-protocol` (Rust) | Apache-2.0 | Link into `renderly-app` | Compatible. Version unverified |
| `@agentclientprotocol/sdk` | Apache-2.0 | Only if the sidecar option is chosen | Compatible |
| `claude-agent-acp` | Apache-2.0 (repo LICENSE); registry says "proprietary" | Launched by the user via `npx`. **Not bundled** | Compatible if not bundled. Resolve the registry conflict first |
| `codex-acp` | Apache-2.0 per registry/README; GitHub says "Other" | Launched by the user via `npx`. Not bundled | Compatible if not bundled. Confirm LICENSE file |
| `@google/gemini-cli` | Apache-2.0 | Launched by the user via `npx`. Not bundled | Compatible if not bundled |
| Antigravity `agy_acp_server` | Proprietary | Downloaded by the user from Google's host | **Never bundled.** Link to the registry only |
| Claude Agent SDK | Commercial Terms | Not used directly; reached only via the adapter the user launches | Do not bundle |

**Rule for Renderly:** never redistribute a vendor CLI, adapter binary, or SDK. The app only **detects** and **launches** what the user has installed. Installer, README and in-app text must not imply that Renderly is affiliated with or endorsed by any vendor (AGPL/§0 rule 9).

---

## 6. Phased plan

### Phase 1: MVP — embedded panel, Claude Code and Codex

- `agent_chat/` module in `src-tauri` with an `AgentDriver` trait (spawn, send prompt, stream
  events, answer permission requests, cancel, shut down) and two drivers: Claude Code
  (stream-json) and Codex (`codex app-server`). Detection of installed CLIs and their sign-in
  state.
- `renderly-mcp --project <path>` startup argument (Q5, approved) so the session starts with the
  open project and edits go live through the bridge.
- The embedded panel (§4.1): transcript, streaming text, tool-call cards with an Undo on edit
  cards, permission prompts with the §4.3 defaults, cancel, agent picker.
- Tests: a fake agent CLI (small test binary speaking each driver's stdio protocol) drives the
  backend in `cargo test`; no vendor CLI or account needed in CI. Vitest for the panel's event
  reducer.

Exit criterion: user types "trim the first clip to 5 seconds" in the panel, the edit appears live
in the timeline, and Ctrl+Z undoes it — with Claude Code and with Codex, each signed in with the
user's own account.

### Phase 2: picker, permissions UI, first-run flow

- Agent picker with detection, version and auth status.
- "Allow edits" toggle, per-action permission history for the session.
- First-run "connect your agent" flow (§4.5).
- `--project` startup argument for `renderly-mcp` (if Q5 approved) to replace prompt bootstrap.
- Per-turn undo grouping (if Q6 decided).

### Phase 3: more agents

- Gemini CLI (`--acp`), using an API key only, unless Q2 says otherwise.
- Claude (`--hide-claude-auth`) if Q1 is resolved.
- Other registry agents, each with a licence check before listing. Antigravity only if Q4 resolves to "supported".

### Explicitly out of scope

- Bundling or redistributing any vendor CLI, adapter, binary or SDK.
- Renderly-hosted model access, key storage, billing, or any proxy of vendor traffic.
- Renderly-side login or OAuth flows for vendor accounts.
- Community ACP bridges for Antigravity (undocumented flags, ToS risk).
- Remote or multi-user agent sessions. The bridge stays loopback-only.
- Agent-driven filesystem or terminal access through ACP client capabilities (kept off in the MVP).
- Any change to `renderly-core` beyond what the existing command API already provides.

---

## 7. Test plan

- Unit: ACP message handling against a fake agent binary (spec shapes from §2.1).
- Unit: permission classifier (every MCP tool name maps to one class; unknown tools map to Ask).
- Integration: `renderly-mcp` launched by the fake agent with `open_project`, `apply_command` over a test bridge; assert `project:changed` and undo.
- Manual, Windows: spawn `npx` adapter through `cmd /c`; cancel mid-turn; close the app and confirm no orphan `node` processes.
- Regression: AGENTS.md definition of done (`cargo build/test`, fmt, clippy `-D warnings`).

---

## 8. Phase-0 spikes before committing to Phase 1

1. Run `npx @agentclientprotocol/codex-acp@2.2.2` from Rust over stdio; complete `initialize`, `session/new` with a stub `mcpServers` entry, and one `session/prompt`. Record the actual field names (the public docs are not precise on `mcpServers` shape).
2. Confirm the Rust crate version and API from crates.io, which was unreachable during research.
3. Confirm `gemini --help` shows `--acp` at the registry version.
4. Confirm the `claude-agent-acp` registry licence conflict with its maintainers.
5. Measure `renderly-mcp` cold start when the agent launches it per session.

---

## 9. Open questions for the owner

| # | Question | Why it matters | Current default |
|---|---|---|---|
| Q1 | ~~**Claude subscription login.** Can Renderly offer "sign in with your Claude subscription" through the adapter, or is that a third-party integration the vendor prohibits? Ask Anthropic sales if unsure~~ | — | Resolved 2026-10-15: drive the installed, signed-in `claude` CLI itself; no adapter, no login handled by Renderly. |
| Q2 | ~~**Gemini Google sign-in.** Google's terms say third-party tools using CLI OAuth violate the terms. Is Renderly "a third-party tool" in that sense?~~ | — | Resolved 2026-10-15: drive Google's own `gemini` CLI (`--acp`) that the user signed in; Renderly handles no OAuth. |
| Q3 | ~~**Codex ChatGPT login.** Is reusing a ChatGPT subscription login through a third-party ACP client permitted? Not found in OpenAI docs~~ | — | Resolved 2026-10-15: drive the installed, signed-in `codex` CLI (`codex app-server`). |
| Q4 | **Antigravity.** Proprietary binary, no documented headless interface, and only community bridges. Is it in scope at all? | Affects the "Antigravity" promise in the parity row | Not in MVP; re-check in Phase 3 |
| Q5 | ~~**Bootstrap.** Approve a `--project <path>` startup argument for `renderly-mcp`? It would change a server that AGENTS.md treats as stable~~ | — | Approved 2026-10-15: add `renderly-mcp --project <path>`. |
| Q6 | **Undo granularity.** One undo step per agent *edit*, per *turn*, or per *user message*? | Shared undo with the human is only useful if agent turns group cleanly | Per edit (matches today's history) |
| Q7 | **Permission defaults.** Accept the §4.3 table, or stricter (ask for every edit)? | Trades speed against control | As written |
| Q8 | **Vendor mode.** Should Renderly ever request the agent's most permissive mode? | Recommendation is no | No |
| Q9 | **Sign-off on data disclosure text** in §5.3 (wording on Anthropic, Google, OpenAI) | Legal-sensitive copy | Draft in §5.3 |
| Q10 | **Shipping `renderly-mcp`.** Does the installer ship `renderly-mcp.exe` next to the app, so the agent can launch it by absolute path? | `command` must be an absolute path (ACP §2.1) | Assumed yes; not verified against the installer config |

---

## 10. Claims not verified

- The Rust `agent-client-protocol` crate version and API. crates.io was unreachable.
- The exact `mcpServers` object shape in `session/new` beyond the stdio fields listed on the session-setup page (Spike 1).
- Codex ChatGPT login behaviour for third-party ACP clients (Q3).
- Whether `claude-agent-acp` at 0.90.0 offers subscription login by default (the flag exists; the default was read from source, not run).
- Gemini `--acp` vs `--experimental-acp` in the installed version (Spike 3).
- Antigravity's Google-sign-in flow and whether it is covered by any documented terms for third-party use.
- Licences and launch commands for the registry entries not named in §2.2.
- `codex-acp` LICENSE file: README and registry say Apache-2.0; GitHub detects "Other".
- Vendor image size limits behind the §5.4 numbers.
- Whether the Claude Agent SDK's permission modes can be set per session through the adapter.
- Windows `npx` launch via `cmd /c` (§3.2) is untested.

---

## 11. Phase 1 status (implemented 2026-10-15)

**Built**

- `renderly-mcp --project <path>` opens the project at startup (same as `open_project`; a bad path
  exits with an error). Documented in [mcp-agent-guide.md](mcp-agent-guide.md).
- `renderly-mcp` tool `permission_prompt`: the `--permission-prompt-tool` target. It forwards the
  request to the app's bridge method `agent_permission` and denies when the app is not live on
  the project.
- Backend `renderly-app/src-tauri/src/agent_chat/` (see [architecture.md](architecture.md)):
  `AgentDriver` trait; Claude Code driver; Codex driver; one `AgentEvent` enum sent as `agent:event`;
  `PermissionBroker` with the §4.3 defaults; `agent_list`, `agent_start`, `agent_send`,
  `agent_answer_permission`, `agent_cancel`, `agent_set_auto_edits` (not in the original command list;
  needed for the auto-edit toggle), `agent_stop`. Stop on project close and on app exit.
- Frontend: Agent tab beside the Inspector in the right column; top-bar toggle; `Ctrl+L`
  (`view.focus-agent`). Picker with install and sign-in state and the exact command to fix each.
  Streamed text, tool cards, an Undo on the latest successful edit, permission prompts, cancel,
  stop, an auto-apply toggle and removable context chips. Reducer in `lib/agentChat.ts` with Vitest.
- Browser harness: scripted fake agent in `dev/tauriMock.ts` (text, a real `TrimClip` through the
  mock's `apply_command`, an export permission prompt).

**Claude Code: how permissions work**

Claude Code is started with `--permission-prompt-tool mcp__renderly__permission_prompt`. For every
tool call not on the pre-allowed list (the Renderly perception tools), it calls that MCP tool. The
tool forwards to the app; the app's policy allows perception tools, allows edits when the toggle is
on, and asks the user for everything else. The reply is JSON `{"behavior":"allow","updatedInput":…}`
or `{"behavior":"deny","message":…}`. This reply shape follows the documented `canUseTool` contract;
it has not been checked against the real binary (see below).

**Codex**

Driven as `codex app-server`, with the Renderly MCP server passed as `-c` overrides so the user's
`config.toml` is not edited. Built from the published docs (`initialize`, `thread/start`,
`turn/start`, `turn/interrupt`, `item/agentMessage/delta`, `item/started`/`item/completed`,
`turn/completed`). **Untested against the real binary.** The approval method names and the response
shape, and the MCP `-c` override, are assumptions. If they are wrong, the session fails to start or
approvals hang until the user cancels; the failure is shown in the panel.

**Does not work yet**

- Vendor binaries are not shipped. `renderly-mcp` must be next to the app executable or in the
  workspace `target` build. The installer does not ship it (Q10 is still open).
- Cancel kills the CLI process tree and the next turn relaunches with `--resume`. There is no
  graceful SIGINT on Windows.
- One session at a time; no per-turn undo grouping (Q6 undecided); no "Always" for file writes or
  shell (by design, §4.3).
- The Inspector and Agent cannot be shown side by side (§4.1 split). The column width is not
  persisted.
- Assistant text is shown as plain text; Markdown is not rendered.
- Gemini and the other agents (Phase 3) are not listed.

**Not verified against the real CLIs (owner must check, see qa-checklist.md)**

- The `--permission-prompt-tool` reply shape and whether the MCP server is connected before the
  first turn.
- Whether `stream_event` text deltas arrive in the form the parser expects.
- `claude auth status` exit codes (documented as 0 signed in, 1 not signed in).
- Codex's `-c` overrides before `app-server`, the `approvalPolicy` value, approval request names and
  payloads, and item payload fields.

**Tests**

- `renderly-mcp`: unit tests for the argument parser, the `--project` stdio cases, and the
  `permission_prompt` denial when no app is live.
- `renderly-app`: permission classification and broker (allow once, allow always scoped to the
  tool and command, reject, cancel-all, no always for file writes); the stream-json and Codex
  parsers; launch-flag contract; `tests/agent_chat.rs` runs both real drivers against the
  `renderly-fake-agent` binary (text, a tool call, a Codex approval allowed and denied, cancel then a
  second turn, kill on stop, missing CLI). The Claude permission path is not exercised end to end in
  tests: it needs the real CLI to call the MCP tool. The broker, the MCP tool's denial path and the
  flag contract are unit-tested instead.
- Vitest: reducer (text streaming, tools, permissions, turns, errors, session ids), edit-card
  summaries, and Undo eligibility.
