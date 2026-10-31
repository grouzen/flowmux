#!/usr/bin/env python3
"""Example Flowmux copy command for Codex, using agent-handoff on both hosts."""

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import uuid


TIMEOUT_SECONDS = 240


def fail(message):
    raise RuntimeError(message)


def run_json(argv):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=TIMEOUT_SECONDS)
    if result.returncode:
        detail = " ".join(part for part in (result.stderr.strip(), result.stdout.strip()) if part)
        fail(f"{' '.join(argv[:2])} failed: {detail or result.returncode}")
    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"{' '.join(argv[:2])} returned invalid JSON: {error}")
    if not isinstance(parsed, dict):
        fail(f"{' '.join(argv[:2])} returned a non-object JSON result")
    return parsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "host",
        "flowmux-session",
        "source-id",
        "agent-name",
        "agent-type",
        "source-session-id",
        "source-directory",
        "destination-directory",
    ):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--protocol-version", required=True, type=int)
    args = parser.parse_args()

    if args.protocol_version != 1:
        fail(f"unsupported Flowmux copy protocol {args.protocol_version}")
    if args.agent_type != "codex":
        fail(f"example copy command supports Codex only, not {args.agent_type}")
    if not args.host or args.host.startswith("-") or any(c.isspace() for c in args.host):
        fail("invalid SSH host")
    if not args.source_session_id:
        fail("source native session ID is empty")
    destination = Path(args.destination_directory).resolve(strict=True)
    if not destination.is_dir():
        fail("destination is not a directory")

    remote_bundle = f"/tmp/flowmux-agent-copy-{uuid.uuid4().hex}.zip"
    share = shlex.join(
        [
            "agent-handoff",
            "share",
            "--source",
            "codex",
            "--thread",
            args.source_session_id,
            "--format",
            "zip",
            "--out",
            remote_bundle,
        ]
    )
    # SSH runs a remote shell. Quote the entire script as one sh -c argument,
    # and quote every value inside it. The trap removes the source bundle.
    script = (
        'set -eu; PATH="$HOME/.local/bin:$PATH"; export PATH; umask 077; '
        f"trap {shlex.quote('rm -f ' + shlex.quote(remote_bundle))} 0; "
        f"{share} >/dev/null; cat {shlex.quote(remote_bundle)}"
    )
    ssh = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "--",
        args.host,
        shlex.join(["sh", "-c", script]),
    ]
    with tempfile.TemporaryDirectory(prefix="flowmux-agent-copy-") as temp_dir:
        bundle = Path(temp_dir) / "source.zip"
        fd = os.open(bundle, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as output:
            transfer = subprocess.run(
                ssh, stdout=output, stderr=subprocess.PIPE, timeout=TIMEOUT_SECONDS
            )
        if transfer.returncode:
            fail(f"remote Codex share failed: {transfer.stderr.decode(errors='replace').strip()}")
        if bundle.stat().st_size == 0:
            fail("remote Codex share returned an empty bundle")

        imported = run_json(
            [
                "agent-handoff",
                "import",
                str(bundle),
                "--target",
                "codex",
                "--cwd",
                str(destination),
                "--execute",
                "--allow-duplicate",
            ]
        )
        new_id = imported.get("thread_id")
        if (
            imported.get("status") != "ok"
            or imported.get("dry_run") is not False
            or not isinstance(new_id, str)
            or not new_id
            or new_id == args.source_session_id
        ):
            fail(f"agent-handoff did not create a fresh Codex thread: {imported}")

        verified = run_json(
            [
                "agent-handoff",
                "verify",
                "--thread",
                new_id,
                "--source",
                "codex",
                "--cwd",
                str(destination),
            ]
        )
        if verified.get("status") != "ok" or verified.get("thread_id") != new_id:
            fail(f"imported Codex thread {new_id} failed verification: {verified}")

    print(json.dumps({"version": 1, "agent_type": "codex", "session_id": new_id}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"agent-copy-codex-example: {error}", file=sys.stderr)
        sys.exit(1)
