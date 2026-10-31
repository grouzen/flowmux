# Agent transfer between flowmux machines

## Purpose

Let a user copy an agent from a flowmux session on another computer into the
current local flowmux session. The source agent remains available. The imported
agent gets a new, independent native session and can diverge from the source.

Flowmux follows the Unix philosophy here: it owns discovery, selection, local
registration, and presentation; a configured external command owns the native
session copy. Flowmux must not implement Codex, Claude Code, OpenCode, or Pi
transcript conversion or synchronize project files.

This document records the decisions from the design interview. The implemented
protocol and Codex copy command example are documented in
[`docs/agent-transfer-protocol.md`](docs/agent-transfer-protocol.md). A real
cross-machine Codex import has been completed; automated coverage still uses
a simulated copy command.

## Agreed user flow

1. Open an import picker in the **local** flowmux UI.
2. Select a saved SSH host or enter a new host. Save a newly entered host after
   the connection succeeds.
3. Select one of the remote machine's running flowmux sessions, then an agent
   recorded in that session's flowmux configuration. Do not list arbitrary
   native CLI sessions that flowmux does not manage.
4. Poll remote live status while the picker is open. Show agents whose types
   are enabled locally. Only an **Idle** agent with a persisted native session
   ID is selectable. Running, Starting, WaitingForInput, Stopped, Unknown, and
   agents without a native session ID remain visible but disabled with a reason.
   If remote flowmux is offline, show the session as offline and disable its
   agents.
5. Select an **existing** local working directory. The import belongs to the
   local project currently selected in flowmux and retains the remote agent's
   name. Project files are expected to be present through an external sync
   process; flowmux does not copy, clone, or create them for this feature.
6. Flowmux invokes one globally configured local copy command with the SSH
   host, remote flowmux session, source agent details (including native type
   and session ID), and selected destination directory. The command is
   responsible for transport and for creating a separate native session on
   the local computer. It must map the copied session to the selected working
   directory.
7. On success, the command returns machine-readable JSON containing the new
   native session ID. Flowmux verifies that the ID differs from the source and
   that the destination CLI can find it, then creates and launches a local
   agent in a new tmux pane. The remote agent stays intact.
8. On failure, show the command's error in the picker and do not create a
   local agent card. An unsupported agent type is an ordinary copy-command
   failure; flowmux does not maintain a hardcoded support matrix.

Repeated imports of the same source are allowed. Each must produce a new,
independent native session, even when the local cards share the original name.

## Boundaries and configuration

- Flowmux performs remote **listing** itself over SSH. The remote host needs a
  read-only flowmux interface that returns its session names, configured agents,
  native session IDs, and current live status as JSON. The exact CLI spelling
  and internal status-sharing mechanism are implementation choices. A plain
  config-file read is insufficient because live status is in the running
  flowmux process, not the per-session TOML.
- Flowmux uses the user's SSH host aliases/configuration. Keep a saved-host
  list in global flowmux config and allow a one-off new host in the picker.
  Save it only after successful connection.
- Add one global setting for the external copy command, analogous in spirit to
  `git_viewer`. The command may be a wrapper around any existing transfer tool
  and must receive the source agent type; local `enabled_agents` controls what
  the picker shows. There is no built-in copy implementation or hardcoded
  choice of transfer tool.
- Run the configured command locally. Pass arguments as distinct process
  arguments, without interpolating source metadata into a shell command.
  Preconfigured command arguments are fine. The exact flag names and JSON
  schema should be documented and versioned before implementation.
- Credentials, agent-wide settings, secrets handling, transport encryption,
  and native session format compatibility belong to the external command and
  the locally installed agent CLIs. Flowmux should surface their failures.
- A remote status can change between polling and selection. Re-read or
  otherwise validate idle status immediately before invoking the copy command;
  the command remains the final authority on whether a safe snapshot exists.

### Proposed command contract (illustrative, not settled syntax)

The implementation should define one stable contract with at least these
inputs: SSH host, remote flowmux session, source agent type, source native
session ID, source working directory, and chosen local working directory.
Including the source agent name and a flowmux record identifier or pane target
may help diagnostics. Use an opaque source identifier rather than parsing a
display name.

For example, a configured `agent-copy` wrapper could receive:

```text
agent-copy --host laptop --flowmux-session flowmux \
  --agent-type codex --source-session-id <source-id> \
  --source-directory <remote-path> --destination-directory <local-path>
```

On success it could print one JSON object to stdout:

```json
{"agent_type":"codex","session_id":"<new-local-id>"}
```

Exit code zero plus a valid, different local session ID is required. A nonzero
exit code, malformed output, a reused source ID, or an ID that the local CLI
cannot resume must fail visibly. Reserve stdout for the result and stderr for
human-readable diagnostics. These are proposed protocol details, not extra
transfer responsibilities for flowmux.

## Repository facts and likely touchpoints

- `src/config.rs`: per-session TOML lives under
  `~/.config/flowmux/sessions/<session>.toml`. `AgentConfig` stores name,
  absolute directory, project, tmux pane, native `AgentKind`, optional native
  session ID, and optional worktree metadata. Local pane targets, ports, and
  absolute paths must not be copied verbatim into a destination agent record.
- `src/global_config.rs`: global config lives at
  `~/.config/flowmux/config.toml`. It currently includes `git_viewer` and
  `enabled_agents`; this is the natural place for saved SSH hosts and the
  configured copy command. The current git viewer launches a configured
  command in a tmux pane (`src/app.rs`), but the new copy command needs a
  structured result and therefore should be invoked as a subprocess with
  captured output, not as an interactive pane.
- `src/main.rs`: current CLI has a `Launch` subcommand but no read-only remote
  listing command. Startup acquires a per-session lock and opens local tmux;
  a remote query must avoid starting another dashboard or taking its lock.
- `src/app.rs`, `src/ui/`: add the import picker, SSH and copy progress/errors,
  periodic remote refresh, disabled reasons, and the action that registers a
  successfully imported agent in the selected local project.
- `src/runner.rs`: centralizes creation and restart. Add a path that attaches
  a **pre-existing local native session** to a new local pane without creating
  a git worktree. Allocate fresh local ports and pane targets as normal.
- `src/models.rs`: `AgentStatus` includes Starting, Running,
  WaitingForInput, Idle, Stopped, and Unknown. The picker requires Idle.
- `src/agents/codex.rs`: Codex restart already accepts a native session ID
  and launches through a new local app-server port. Other agent adapters
  have their own resume paths. Verify a returned ID using the relevant native
  adapter rather than assuming a copied file is enough.
- Flowmux's current status cache is in memory. Exposing it to a separate
  read-only CLI process may require a local status socket or an atomic
  snapshot. Define freshness and offline behavior; do not infer Idle from a
  stale config file.

## Suggested implementation sequence

1. Define and document versioned JSON for remote session/agent listing and
   the configured copy command. Keep IDs, types, status, and errors explicit.
2. Add a read-only remote flowmux CLI query and a way for it to obtain live
   status from the running instance. Verify it works while the dashboard owns
   the session lock and that offline instances are reported honestly.
3. Add saved SSH hosts and the global copy-command setting. Invoke `ssh` and
   the copy command with argument arrays and bounded timeouts; capture errors.
4. Add the local picker: host, remote session, eligible agent, and existing
   destination directory. Refresh status periodically and revalidate before
   copying.
5. Add the runner path for a returned, already-copied native session. Persist
   a fresh local record only after validation and successful launch. Resolve
   source worktree metadata against the selected local Git worktree, and do
   not claim ownership of a pre-existing worktree or remote path.
6. Document the external command contract and provide a small **example**
   wrapper or recipe for Codex using an existing tool. Keep the example
   separate from flowmux's core transfer logic.

## Acceptance criteria

- A local user can browse a chosen remote host's running flowmux sessions and
  see only the remote config's agents whose types are locally enabled.
- Status updates while the picker remains open. Only Idle agents with native
  session IDs can be selected; offline/unknown state never appears idle.
- A successful copy creates a new native session ID and a new local agent card
  in the current project, with the remote name and chosen local directory.
  Both source and destination can continue independently.
- Importing the same source again creates another independent session.
- The external command receives the native agent type and handles unsupported
  types with a surfaced error. Flowmux contains no per-tool transcript-copy
  logic and transfers no project files.
- SSH, malformed JSON, command failure, stale status, unchanged session ID,
  missing local session, and launch failure leave no misleading agent card.
  If a copy succeeds but later registration fails, report the new native ID
  for manual recovery; do not silently delete the copied history.
- Remote listing works while remote flowmux is running and reports offline
  sessions without treating saved records as live agents.

## Existing tools and research

These are candidates for a **user-configured command or wrapper**, not tools
that flowmux should reimplement or commit to as a required dependency.

| Tool | Relevant capability | Limit for this design |
| --- | --- | --- |
| [codex-claude-transfer (`cct`)](https://github.com/ahmojo/codex-claude-transfer) | Codex and Claude session bundles, JSON output, directory mapping, optional experimental LAN sync | Its LAN sync is not the exact remote-list / selected-agent / guaranteed-new-ID contract; a wrapper or extra commands may be needed. Its `--import-as-copy` has limitations for inherited Codex forks. |
| [agent-handoff](https://github.com/DavidDingXu/agent-handoff) | Imports Codex/Claude history as a new native task from a file or encrypted link | Needs a wrapper for flowmux's SSH host and source-session contract. |
| [txcript](https://github.com/skillsynchq/txcript) | Exports and writes native sessions for Codex, Claude Code, OpenCode, and Pi through its Simple interchange format | A same-agent `continue <id>` resumes the source, so a copy must use `export` then `continue <file> --no-resume`; the CLI prints the new ID in human-readable output rather than JSON, and the wrapper must set the selected destination directory in the exported document. Conversion through Simple can lose agent-specific fields. |
| [codex-sync](https://github.com/Jac0bXu/codex-sync) | Uses rsync over SSH to copy Codex session files | Syncing files alone does not guarantee an independent fork or local flowmux registration. |
| [Agent Sessions Sync](https://github.com/Gregor-von-Vitek/agent-sessions-sync) | Syncs Claude, Codex, and Cursor histories through private GitHub | Background history sync, not selective flowmux agent import. |
| [OpenCode CLI](https://dev.opencode.ai/docs/cli/) | Native session export/import | Useful for a future external adapter; flowmux need not parse OpenCode storage. |

Codex itself has a native `fork` command, which is relevant to the independent
copy requirement: [Codex CLI source](https://github.com/openai/codex/blob/main/codex-rs/exec/src/cli.rs).
For `cct`'s current behavior and caveats, consult its
[usage guide](https://github.com/ahmojo/codex-claude-transfer/blob/Main/docs/usage.md)
and [command reference](https://github.com/ahmojo/codex-claude-transfer/blob/Main/docs/reference.md)
before writing an example wrapper. Native agent formats and tool capabilities
may change; validate against current installed versions during implementation.
