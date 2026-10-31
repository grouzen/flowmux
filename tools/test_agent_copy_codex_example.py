"""Contract smoke test for the Codex copy command example."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("agent-copy-codex-example.py")


class CodexCopyExampleTest(unittest.TestCase):
    def test_share_import_verify_returns_fresh_flowmux_result(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            ssh = bin_dir / "ssh"
            ssh.write_text(
                "#!/usr/bin/env python3\n"
                "import os, subprocess, sys\n"
                "env = dict(os.environ, HOME=os.environ['REMOTE_HOME'], PATH='/usr/bin:/bin')\n"
                "result = subprocess.run(sys.argv[-1], shell=True, env=env)\n"
                "sys.exit(result.returncode)\n"
            )
            ssh.chmod(0o755)
            handoff = bin_dir / "agent-handoff"
            handoff.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, pathlib, sys\n"
                "args = sys.argv[1:]\n"
                "if args[0] == 'share':\n"
                "    assert args[args.index('--thread') + 1] == os.environ['EXPECTED_SOURCE_ID']\n"
                "    bundle = pathlib.Path(args[args.index('--out') + 1])\n"
                "    bundle.write_bytes(b'bundle')\n"
                "    pathlib.Path(os.environ['BUNDLE_PATH_LOG']).write_text(str(bundle))\n"
                "elif args[0] == 'import':\n"
                "    assert pathlib.Path(args[1]).read_bytes() == b'bundle'\n"
                "    assert '--allow-duplicate' in args and '--execute' in args\n"
                "    print(json.dumps({'status': 'ok', 'dry_run': False, 'thread_id': 'fresh'}))\n"
                "elif args[0] == 'verify':\n"
                "    print(json.dumps({'status': 'ok', 'thread_id': args[args.index('--thread') + 1]}))\n"
            )
            handoff.chmod(0o755)
            remote_home = root / "remote-home"
            remote_bin = remote_home / ".local" / "bin"
            remote_bin.mkdir(parents=True)
            (remote_bin / "agent-handoff").symlink_to(handoff)
            bundle_path_log = root / "bundle-path"
            marker = root / "unexpected-command"
            source_id = f"source'$(touch {marker})"
            env = {
                **os.environ,
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
                "BUNDLE_PATH_LOG": str(bundle_path_log),
                "EXPECTED_SOURCE_ID": source_id,
                "REMOTE_HOME": str(remote_home),
            }
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--protocol-version", "1",
                    "--host", "example",
                    "--flowmux-session", "remote",
                    "--source-id", "pane",
                    "--agent-name", "worker",
                    "--agent-type", "codex",
                    "--source-session-id", source_id,
                    "--source-directory", "/remote",
                    "--destination-directory", str(root),
                ],
                capture_output=True,
                text=True,
                env=env,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout.strip(),
                '{"version": 1, "agent_type": "codex", "session_id": "fresh"}',
            )
            self.assertFalse(Path(bundle_path_log.read_text()).exists())
            self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
