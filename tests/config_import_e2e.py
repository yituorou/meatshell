"""CLI/MCP import regression checks using synthetic data and isolated profiles.

Run: python tests/config_import_e2e.py --exe target/debug/meatshell
No server connections or real credentials are used.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


# Synthetic portable export credential, generated with MeatShell v1 export format.
PORTABLE_PASSWORD = 'enc:exp:v1:AAAAAAAAAAAAAAAAFeRqZmIDNa2U57LDoinkeMgDVXvorTTv3qrwXh1pSN4M7bdXvSrE'


def session(name, **overrides):
    value = dict(id=name, name=name, host=f"{name}.invalid", port=22,
                 user="fixture", auth="password", password="synthetic-only-password",
                 kind="ssh", group="fixture group")
    value.update(overrides)
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    exe = parser.parse_args().exe.resolve()
    with tempfile.TemporaryDirectory(prefix="meatshell-import-") as directory:
        root = Path(directory)
        profile = root / "profile"
        profile.mkdir()
        config = profile / "sessions.json"
        config.write_text(json.dumps(dict(sessions=[session("existing")], defaults_rev=3,
            mcp_enabled=True, mcp_use_saved_credentials=True, mcp_allow_commands=False,
            mcp_allow_file_transfers=True)), encoding="utf-8")
        export = root / "export.json"
        export.write_text(json.dumps(dict(meatshell_export=1, sessions=[
            session("target", password=PORTABLE_PASSWORD, jump_session_id="inner", jump_session_ids=["outer", "inner"]),
            session("inner", jump_session_id="outer"), session("outer")
        ])), encoding="utf-8")

        def cli(*args, ok=True):
            run = subprocess.run([str(exe), "--data-dir", str(profile), "cli", *args],
                                 capture_output=True, text=True, timeout=30)
            assert (run.returncode == 0) == ok, (run.returncode, run.stdout, run.stderr)
            assert "synthetic-only-password" not in run.stdout + run.stderr
            return json.loads(run.stdout) if ok else run

        def mcp(arguments, allow=False, tool="import_sessions"):
            request = dict(jsonrpc="2.0", id=1, method="tools/call",
                           params=dict(name=tool, arguments=arguments))
            args = [str(exe), "--data-dir", str(profile), "mcp", "serve"]
            if allow:
                args.append("--allow-config-import")
            run = subprocess.run(args, input=json.dumps(request)+"\n", capture_output=True,
                                 text=True, timeout=30)
            assert run.returncode == 0, run.stderr
            assert "synthetic-only-password" not in run.stdout + run.stderr
            return json.loads(run.stdout)["result"]

        # Let normal configuration initialization happen before comparing bytes.
        cli("sessions", "--json")
        before = config.read_bytes()
        assert cli("import", str(export), "--dry-run", "--json") == dict(added=3, skipped=0, dry_run=True)
        assert config.read_bytes() == before
        preview = mcp(dict(local_path=str(export)))
        assert preview["structuredContent"] == dict(added=3, skipped=0, dry_run=True)
        assert config.read_bytes() == before
        denied = mcp(dict(local_path=str(export), dry_run=False))
        assert denied["isError"] and "--allow-config-import" in denied["content"][0]["text"]
        assert config.read_bytes() == before
        print("PASS: CLI/MCP preview and MCP write opt-in")

        assert cli("import", str(export), "--json") == dict(added=3, skipped=0, dry_run=False)
        saved = json.loads(config.read_text())
        assert saved["mcp_allow_commands"] is False
        by_name = {item["name"]: item for item in saved["sessions"]}
        assert by_name["existing"]["id"] == "existing"
        assert by_name["target"]["jump_session_ids"] == [by_name[x]["id"] for x in ("outer", "inner")]
        assert by_name["target"]["jump_session_id"] == by_name["inner"]["id"]
        assert by_name["inner"]["jump_session_id"] == by_name["outer"]["id"]
        assert by_name["target"]["password"].startswith("enc:v1:")
        assert "synthetic-only-password" not in config.read_text()
        metadata = cli("sessions", "--json")
        assert all(s["has_saved_password"] for s in metadata["sessions"])
        assert all(s["group"] == "fixture group" for s in metadata["sessions"])
        assert cli("import", str(export), "--json") == dict(added=0, skipped=3, dry_run=False)
        assert mcp(dict(local_path=str(export), dry_run=False), allow=True)["structuredContent"] == dict(added=0, skipped=3, dry_run=False)
        print("PASS: append-only import, secret storage, settings preservation, forward/back jumps, idempotence")

        distinct = root / "distinct.json"
        incoming = json.loads(export.read_text())
        alias = dict(incoming["sessions"][0], id="target-alias", name="Intentional target alias")
        incoming["sessions"].append(alias)
        distinct.write_text(json.dumps(incoming), encoding="utf-8")
        assert cli("import", str(distinct), "--json") == dict(added=1, skipped=3, dry_run=False)
        assert cli("import", str(distinct), "--json") == dict(added=0, skipped=4, dry_run=False)
        aliases = json.loads(config.read_text())["sessions"]
        assert len([s for s in aliases if s["host"] == "target.invalid"]) == 2
        assert next(s for s in aliases if s["name"] == "target")["id"] == by_name["target"]["id"]
        print("PASS: same-endpoint named aliases survive without replacing existing IDs")

        before = config.read_bytes()
        broken = root / "broken.json"
        for value in ["{broken", json.dumps(dict(meatshell_export=99, sessions=[])),
                      json.dumps(dict(meatshell_export=1, sessions=[session("bad", auth="synthetic-only-password")])),
                      json.dumps(dict(meatshell_export=1, sessions=[session("bad", password="enc:exp:v1:broken")])),
                      json.dumps(dict(meatshell_export=1, sessions=[session("bad", jump_session_id="missing")]))]:
            broken.write_text(value, encoding="utf-8")
            cli("import", str(broken), "--json", ok=False)
            assert config.read_bytes() == before
        cli("import", str(export), "--unknown", ok=False)
        assert mcp(dict(local_path=str(export), dry_run="false"))["isError"]
        print("PASS: malformed, unsupported, invalid-secret and invalid-jump imports are non-mutating and redacted")

        # Opt-in MCP apply uses the same implementation, preserving source settings.
        native = root / "native.json"
        native.write_text(json.dumps(dict(sessions=[session("mcp")], mcp_allow_commands=True)), encoding="utf-8")
        applied = mcp(dict(local_path=str(native), dry_run=False), allow=True)
        assert applied["structuredContent"] == dict(added=1, skipped=0, dry_run=False)
        assert json.loads(config.read_text())["mcp_allow_commands"] is False
        if os.name == "posix":
            assert config.stat().st_mode & 0o777 == 0o600
        print("PASS: MCP apply, native sessions-only import and owner-only output")

        before = config.read_bytes()
        for permission in ("mcp_enabled", "mcp_allow_file_transfers"):
            denied_profile = json.loads(before)
            denied_profile[permission] = False
            config.write_text(json.dumps(denied_profile), encoding="utf-8")
            denied_before = config.read_bytes()
            assert mcp(dict(local_path=str(export)))["isError"]
            assert mcp(dict(local_path=str(export), dry_run=False), allow=True)["isError"]
            assert config.read_bytes() == denied_before
        config.write_bytes(before)
        print("PASS: MCP enable/file-transfer gates remain enforced for preview and apply")


if __name__ == "__main__":
    main()
