# Agent transfer protocol, version 1

This document fixes the wire contract used by the agent transfer feature.

In the local dashboard, press `i` to open the import picker. Enter or select an
SSH host, a remote Flowmux session, an idle agent with a native session ID, and
an existing local directory. The import belongs to the currently selected
local project. The picker refreshes remote status while it is open and checks
the selected source again before running the copy command.

## Remote listing

Run `flowmux transfer list` on the source computer, commonly through SSH:

```text
ssh -o BatchMode=yes laptop flowmux transfer list
```

On the destination computer, `flowmux transfer remote-list --host laptop`
performs this SSH request, validates the JSON version, and saves `laptop` in
the global host list after the request succeeds. It uses SSH host aliases and
configuration, with a 20-second overall timeout.

The dashboard picker asks for a password or key passphrase if the first SSH
listing fails authentication. The credential is kept only in the open picker
and supplied to Flowmux's SSH listing and source recheck. The configured copy
command receives no credential and must use its own SSH authentication setup.

The command writes one JSON object to stdout, with no dashboard startup or
session lock. Its shape is:

```json
{
  "version": 1,
  "sessions": [
    {
      "name": "flowmux",
      "online": true,
      "agents": [
        {
          "id": "flowmux:1.0",
          "name": "research",
          "agent_type": "codex",
          "session_id": "native-session-id",
          "directory": "/home/user/project",
          "status": "idle"
        }
      ]
    }
  ]
}
```

`id` is an opaque identifier within its Flowmux session. Do not parse it or
display it as the agent name. `agent_type` is one of `opencode`, `claude`,
`codex`, or `pi`. `session_id` may be null. `status` is one of `starting`,
`running`, `waiting_for_input`, `idle`, `stopped`, or `unknown`.

The running dashboard publishes a status snapshot every 500 milliseconds. A
snapshot is accepted for at most three seconds. Missing, stale, malformed, or
unknown-version snapshots make the session offline. Agents without a matching
live status are `unknown`. The listing includes saved sessions even when they
are offline. An importer may select only an online `idle` agent with a nonempty
native session ID and a locally enabled agent type. It must query again just
before invoking the copy command.

## Configured copy command

The global `~/.config/flowmux/config.toml` settings are:

```toml
transfer_hosts = ["laptop"]
agent_copy_command = ["/path/to/agent-copy", "--fixed-option"]
```

The first `agent_copy_command` element is the executable. Subsequent elements
are fixed arguments. Flowmux will append these arguments as separate process
arguments, in this order:

```text
--protocol-version 1
--host <SSH host alias or name>
--flowmux-session <remote Flowmux session name>
--source-id <opaque agent id from listing>
--agent-name <remote agent display name>
--agent-type <native agent type>
--source-session-id <remote native session id>
--source-directory <remote working directory>
--destination-directory <existing local working directory>
```

The command runs on the destination computer. It is responsible for copying
the native session, creating an independent new native session, and mapping it
to the destination directory. Flowmux does not transfer project files.
Standard output is reserved for exactly one JSON object:

```json
{"version":1,"agent_type":"codex","session_id":"new-native-session-id"}
```

The command must exit zero on success. It should write human-readable failure
details to stderr and exit nonzero on failure. Flowmux will reject malformed
JSON, mismatched versions or agent types, an empty or unchanged ID, and a
session ID that the local agent CLI cannot resume. The command is the final
authority on whether a safe snapshot of the source can be made.

The backend re-queries the source before invoking the copy command, rejects a
changed source identity or a non-idle status, and bounds command runtime at
five minutes. Flowmux checks the local native session using the relevant agent
adapter, launches it in a new tmux pane, and saves the card only after those
steps succeed. If the copy succeeds but a later step fails, the picker reports
the new native session ID for manual recovery.

## Codex copy command example

[`tools/agent-copy-codex-example.py`](../tools/agent-copy-codex-example.py)
implements the version 1 command contract using
[`agent-handoff`](https://github.com/DavidDingXu/agent-handoff#commands). Install
the `agent-handoff` CLI on both computers, including the source computer's
noninteractive SSH `PATH`. The example adds `~/.local/bin` to that remote
`PATH`, which covers the CLI's usual per-user install location. Set up
noninteractive SSH for the copy command (for example, an unlocked key in
`ssh-agent`). The picker credential is used only by Flowmux's own listing
request. Set the destination computer's global config to:

```toml
agent_copy_command = ["python3", "/absolute/path/to/flowmux/tools/agent-copy-codex-example.py"]
```

The example exports the selected Codex thread on the source, streams a
temporary zip over SSH, imports it locally with `--execute --allow-duplicate`,
and verifies the fresh thread and destination directory before returning the
Flowmux JSON result. Temporary bundles are removed on normal completion and
handled failures. It rejects other agent types. Its argument handling and
output have a simulated end-to-end test: `python3 -m unittest
tools/test_agent_copy_codex_example.py`.

This example has not been tested against an installed `agent-handoff` or a real
remote Codex session. Check the installed tool's CLI and JSON result format,
then test a real round trip before relying on it. Flowmux does not invoke
`agent-handoff` unless this command is configured.

[`txcript`](https://github.com/skillsynchq/txcript) is another possible copy
command backend and currently reads and writes Codex, Claude Code, OpenCode,
and Pi sessions. A Flowmux wrapper would export the selected source to a
Simple document, set that document's `cwd` to the selected local directory,
then run `txcript continue <file> --with <txcript-target> --no-resume` (for
example, `claude_code` for Flowmux's `claude`). It must obtain
the fresh native ID and return Flowmux's version 1 JSON. The current CLI prints
the resume command with that ID as human-readable text, so a robust wrapper
needs a machine-readable result from txcript or its library API. The Simple
conversion can also omit agent-specific fields, including some Codex metadata.

The alternative [`cct`](https://github.com/ahmojo/codex-claude-transfer/blob/Main/docs/usage.md)
supports session export and import, but its `--import-as-copy` applies on
conflict and cannot safely copy Codex forks with inherited history. A wrapper
must not assume a plain `cct import` always yields a new independent ID.
