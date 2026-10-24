# Agent transfer protocol, version 1

This document fixes the wire contract used by the agent transfer feature.
The remote listing command, SSH transport, and copy-command backend are
implemented. The local picker, native session verification, and registration
are subsequent implementation steps.

## Remote listing

Run `flowmux transfer list` on the source computer, commonly through SSH:

```text
ssh -o BatchMode=yes laptop flowmux transfer list
```

On the destination computer, `flowmux transfer remote-list --host laptop`
performs this SSH request, validates the JSON version, and saves `laptop` in
the global host list after the request succeeds. It uses SSH host aliases and
configuration, with a 20-second overall timeout.

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
five minutes. Native session verification and local agent registration are
required before the returned ID becomes a Flowmux card.
