"""Loopback-only multi-hop regression checks. Requires paramiko.
Run: python tests/ssh_jump_chain_e2e.py --exe /path/to/meatshell.exe
Network stalls: add --stage-timeouts (45 seconds for three stage deadlines).
Before-fix reproduction: add --expect-unbounded-stages (90 seconds).
All keys are generated per run; no real credentials or external servers are used.
All fixture configuration and executable copies are deleted after the run.
"""
import argparse
from contextlib import contextmanager
import io
import json
import logging
import os
import pathlib
import select
import shutil
import socket
import stat
import subprocess
import tempfile
import threading
import time
import paramiko
from cryptography.hazmat.primitives import serialization

logging.getLogger("paramiko").setLevel(logging.CRITICAL)
PASSWORD = "loopback-fixture-only"
PASSPHRASE = "loopback-key-passphrase-only"

def background(fn, *args):
    threading.Thread(target=fn, args=args, daemon=True).start()

class Files(paramiko.SFTPServerInterface):
    def list_folder(self, path):
        item = paramiko.SFTPAttributes()
        item.filename = "fixture.txt"
        item.st_mode = stat.S_IFREG | 0o600
        item.st_size = len(b"multi-hop-sftp\n")
        item.st_uid = item.st_gid = 0
        item.st_atime = item.st_mtime = 0
        return [item]

    def stat(self, path):
        return self.list_folder(path)[0]

    lstat = stat

    def open(self, path, flags, attr):
        handle = paramiko.SFTPHandle(flags)
        handle.readfile = io.BytesIO(b"multi-hop-sftp\n")
        return handle

class Server(paramiko.ServerInterface):
    def __init__(self, node):
        self.node = node
        self.destinations = {}

    def get_allowed_auths(self, username):
        return "publickey" if self.node.key_auth else "password,keyboard-interactive"

    def check_auth_publickey(self, username, key):
        self.node.auth_attempts.append("publickey")
        if self.node.stall_auth:
            self.node.release_stalls.wait(40)
        if self.node.key_auth and username == "fixture" and key == self.node.client_key:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_password(self, username, password):
        self.node.auth_attempts.append("password")
        if self.node.interactive or self.node.key_auth:
            return paramiko.AUTH_FAILED
        return self.check_auth_interactive_response([password])
    def check_auth_interactive(self, username, submethods):
        return paramiko.InteractiveQuery("", "", ("Password: ", False))

    def check_auth_interactive_response(self, responses):
        self.node.auth_attempts.append("keyboard-interactive")
        if not self.node.key_auth and responses == [PASSWORD]:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind, chanid):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_direct_tcpip_request(self, chanid, origin, destination):
        if self.node.stall_forward:
            self.node.release_stalls.wait(40)
        if destination not in self.node.routes:
            return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED
        self.destinations[chanid] = self.node.routes[destination]
        self.node.forwarded.append(destination)
        return paramiko.OPEN_SUCCEEDED

    def check_channel_exec_request(self, channel, command):
        def reply():
            time.sleep(0.05)
            try:
                channel.sendall(("multi-hop-command:" + self.node.name + "\n").encode())
                channel.send_exit_status(0)
                channel.close()
            except (EOFError, OSError, paramiko.SSHException):
                pass  # Negative-path tests deliberately close active channels.
        background(reply)
        return True

class Node:
    def __init__(self, name):
        self.name = name
        self.key = paramiko.RSAKey.generate(2048)
        self.client_key = paramiko.RSAKey.generate(2048)
        self.key_auth = False
        self.rsa_sha256_only = False
        self.stall_auth = False
        self.stall_forward = False
        self.release_stalls = threading.Event()
        self.auth_attempts = []
        self.routes = {}
        self.forwarded = []
        self.interactive = False
        self.transports = []
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        background(self.accept)

    def accept(self):
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            background(self.serve, conn)

    def serve(self, conn):
        disabled = {"pubkeys": ["rsa-sha2-512", "ssh-rsa"]} if self.rsa_sha256_only else None
        transport = paramiko.Transport(conn, disabled_algorithms=disabled)
        self.transports.append(transport)
        transport.add_server_key(self.key)
        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Files)
        server = Server(self)
        try:
            transport.start_server(server=server)
            while transport.is_active():
                channel = transport.accept(0.2)
                if channel and channel.chanid in server.destinations:
                    background(self.relay, channel, server.destinations[channel.chanid])
        except (EOFError, OSError, paramiko.SSHException):
            pass
        finally:
            transport.close()

    @staticmethod
    def relay(channel, address):
        target = socket.create_connection(address, timeout=5)
        try:
            while True:
                ready, _, _ = select.select([channel, target], [], [], 5)
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    (target if source is channel else channel).sendall(data)
        except (OSError, EOFError):
            pass
        finally:
            target.close()
            try:
                channel.close()
            except (EOFError, OSError):
                pass  # The fixture's outer transport may already be closed.

    def close(self):
        self.release_stalls.set()
        self.listener.close()
        for transport in self.transports:
            transport.close()

def session(name, host, port, jump=""):
    return dict(id=name, name=name, host=host, port=port, user="fixture",
                auth="password", password=PASSWORD, kind="ssh", jump_session_id=jump)

class Fixture:
    def __init__(self, binary, directory):
        self.root = pathlib.Path(directory)
        self.explicit_profile = True
        self.exe = self.root / "meatshell.exe"
        shutil.copy2(binary, self.exe)
        self.config = self.root / "config"
        self.config.mkdir()
        self.nodes = [Node("outer"), Node("inner"), Node("target")]
        outer, inner, target = self.nodes
        outer.routes[("inner.invalid", 22)] = ("127.0.0.1", inner.port)
        inner.routes[("target.invalid", 22)] = ("127.0.0.1", target.port)
        self.sessions = [
            session("outer", "127.0.0.1", outer.port),
            session("inner", "inner.invalid", 22, "outer"),
            session("target", "target.invalid", 22, "inner"),
        ]
        self.save()
        self.trust()

    def save(self):
        # defaults_rev avoids application migrations writing its user backup.
        value = dict(sessions=self.sessions, defaults_rev=3,
                     mcp_enabled=True, mcp_use_saved_credentials=True,
                     mcp_allow_commands=True, mcp_allow_file_transfers=True)
        (self.config / "sessions.json").write_text(json.dumps(value), encoding="utf-8")

    def trust(self, omit=None):
        entries = []
        for item, node in zip(self.sessions, self.nodes):
            if item["id"] != omit:
                entries.append(f'{item["host"]}:{item["port"]} {node.key.get_name()} {node.key.get_base64()}')
        (self.config / "known_hosts").write_text("\n".join(entries)+"\n", encoding="utf-8")

    def use_key(self, index, inline=True, encrypted=False, encoding="openssh"):
        node = self.nodes[index]
        node.key_auth = True
        encryption = (serialization.BestAvailableEncryption(PASSPHRASE.encode())
                      if encrypted else serialization.NoEncryption())
        key_format = (serialization.PrivateFormat.TraditionalOpenSSL if encoding == "pem"
                      else serialization.PrivateFormat.OpenSSH)
        key = node.client_key.key.private_bytes(
            serialization.Encoding.PEM, key_format, encryption
        ).decode("utf-8")
        item = self.sessions[index]
        item.update(auth="key", password=PASSPHRASE if encrypted else "",
                    private_key_path="", private_key_inline="")
        if inline:
            item["private_key_inline"] = key
        else:
            path = self.root / f"{node.name}-client-key.pem"
            path.write_text(key, encoding="utf-8")
            path.chmod(0o600)
            item["private_key_path"] = str(path)
        self.save()

    def reset_auth(self):
        for item, node in zip(self.sessions, self.nodes):
            node.key_auth = node.interactive = node.rsa_sha256_only = False
            node.stall_auth = node.stall_forward = False
            item.update(auth="password", password=PASSWORD,
                        private_key_path="", private_key_inline="")
        self.save()

    def mcp(self, name, **arguments):
        requests = [
            dict(jsonrpc="2.0", id=1, method="initialize", params=dict(
                protocolVersion="2025-06-18", capabilities={},
                clientInfo=dict(name="jump-chain-regression", version="1"))),
            dict(jsonrpc="2.0", method="notifications/initialized", params={}),
            dict(jsonrpc="2.0", id=2, method="tools/call",
                 params=dict(name=name, arguments=arguments)),
        ]
        profile_args = ["--data-dir", str(self.config)] if self.explicit_profile else []
        run = subprocess.run([str(self.exe), *profile_args, "mcp", "serve"], input="".join(
            json.dumps(r)+"\n" for r in requests), text=True, capture_output=True,
            encoding="utf-8", timeout=45)
        assert run.returncode == 0, run.stderr
        replies = [json.loads(line) for line in run.stdout.splitlines() if line.strip()]
        return next(reply["result"] for reply in replies if reply["id"] == 2)

    def command(self, name="target"):
        result = self.mcp("run_command", session_id=name, command="fixture",
                          timeout_seconds=8)
        assert not result.get("isError"), result
        value = result["structuredContent"]
        assert value["exit_code"] == 0 and value["stdout"].startswith("multi-hop-command:"), value
        return value

    def check(self, regression_only=False):
        self.explicit_profile = not regression_only
        if regression_only:
            result = self.mcp("run_command", session_id="target", command="fixture",
                              timeout_seconds=5)
            assert result.get("isError"), "Old binary unexpectedly supports nested jumps"
            print("PASS: original binary reproduces nested-jump failure")
            return
        self.check_keys()
        self.command("outer")
        self.command("inner")
        self.command()
        assert ("inner.invalid", 22) in self.nodes[0].forwarded
        assert ("target.invalid", 22) in self.nodes[1].forwarded
        print("PASS: direct, one-hop and two-hop command execution")
        self.nodes[1].interactive = True
        self.command()
        self.nodes[1].interactive = False
        print("PASS: nested keyboard-interactive fallback rebuilds full route")
        for tool, path in [("list_remote_files", "."), ("read_remote_text_file", "/fixture.txt")]:
            self.nodes[2].interactive = tool == "list_remote_files"
            result = self.mcp(tool, session_id="target", path=path, timeout_seconds=8)
            assert not result.get("isError"), result
            assert "fixture" in json.dumps(result), result
        print("PASS: two-hop SFTP listing and reading")
        cli = subprocess.run([str(self.exe), "--data-dir", str(self.config), "cli", "exec", "target", "--json", "--",
                              "fixture"], capture_output=True, text=True, encoding="utf-8", timeout=30)
        assert cli.returncode == 0 and "multi-hop-command:target" in cli.stdout, cli.stderr
        print("PASS: CLI uses the same nested route")
        # Editor-owned routes must match their displayed first-hop-first order,
        # even when a hop still has a legacy reference of its own.
        self.sessions[2]["jump_session_ids"] = ["outer", "inner"]
        self.sessions[2]["jump_session_id"] = ""
        self.save()
        self.command()
        result = self.mcp("list_remote_files", session_id="target", path=".", timeout_seconds=8)
        assert not result.get("isError"), result
        self.sessions[2]["jump_session_ids"] = ["inner", "outer"]
        self.save()
        result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=5)
        assert result.get("isError"), "Reversed editor route was ignored"
        self.sessions[2]["jump_session_ids"] = []
        self.sessions[2]["jump_session_id"] = "inner"
        self.save()
        print("PASS: explicit GUI order drives command/SFTP and rejects a reversed route")
        route = self.nodes[1].routes.pop(("target.invalid", 22))
        result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=5)
        assert result.get("isError"), "Refused inner forwarding was bypassed"
        self.nodes[1].routes[("target.invalid", 22)] = route
        print("PASS: denied inner forwarding fails without direct fallback")
        for host in ("outer", "inner", "target"):
            self.trust(omit=host)
            result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=5)
            assert result.get("isError"), f"Untrusted host accepted: {host}"
        self.trust()
        print("PASS: host key verification is enforced at all three hosts")
        original = self.sessions[0]["jump_session_id"]
        for invalid in ("target", "missing"):
            self.sessions[0]["jump_session_id"] = invalid
            self.save()
            count = sum(len(n.forwarded) for n in self.nodes)
            result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=5)
            assert result.get("isError"), result
            assert count == sum(len(n.forwarded) for n in self.nodes)
        self.sessions[0]["jump_session_id"] = original
        self.save()
        print("PASS: cycles and missing ancestors fail before network activity")
        silent = socket.socket()
        silent.bind(("127.0.0.1", 0))
        silent.listen()
        previous = self.sessions[0]["port"]
        self.sessions[0]["port"] = silent.getsockname()[1]
        self.save()
        try:
            result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=1)
            assert result.get("structuredContent", {}).get("timed_out"), result
        finally:
            silent.close()
            self.sessions[0]["port"] = previous
            self.save()
        print("PASS: timeout covers stalled ancestor SSH handshake")

    def check_keys(self):
        for index in (0, 1):
            cases = ((True, False, "pem"), (False, False, "pem"),
                     (True, False, "openssh"), (False, False, "openssh"),
                     (True, True, "openssh"), (False, True, "openssh"))
            for inline, encrypted, encoding in cases:
                self.reset_auth()
                self.use_key(index, inline=inline, encrypted=encrypted, encoding=encoding)
                before = len(self.nodes[index].auth_attempts)
                self.command()
                assert "publickey" in self.nodes[index].auth_attempts[before:]
                if encrypted:
                    self.sessions[index]["password"] = "wrong-loopback-passphrase"
                    self.save()
                    result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=8)
                    assert result.get("isError"), "Incorrect key passphrase was accepted"
                    assert not result.get("structuredContent", {}).get("timed_out"), result
                print(f"PASS: {self.nodes[index].name} RSA {encoding} {'inline' if inline else 'file'} "
                      f"{'encrypted key + wrong-passphrase rejection' if encrypted else 'key'}")
        self.reset_auth()
        self.use_key(0)
        self.use_key(1, inline=False, encrypted=True)
        self.use_key(2, inline=True, encrypted=True)
        self.command()
        result = self.mcp("list_remote_files", session_id="target", path=".", timeout_seconds=8)
        assert not result.get("isError"), result
        print("PASS: distinct private keys on outer, inner and target; command + SFTP")
        for node in self.nodes:
            node.rsa_sha256_only = True
        self.command()
        print("PASS: all private-key hops with servers advertising RSA-SHA256 only")
        self.nodes[1].key_auth = False
        self.nodes[1].interactive = True
        self.sessions[1].update(auth="password", password=PASSWORD)
        self.save()
        self.command()
        print("PASS: private-key outer/target with inner keyboard-interactive fallback")
        self.reset_auth()

    def check_stage_timeouts(self, expect_unbounded=False):
        # The operation deadline is deliberately longer than the stage deadline.
        # A generic command timed_out here would hide a broken stage deadline.
        def check(expected):
            started = time.monotonic()
            result = self.mcp("run_command", session_id="target", command="fixture", timeout_seconds=30)
            elapsed = time.monotonic() - started
            timed_out = result.get("structuredContent", {}).get("timed_out")
            if expect_unbounded:
                assert timed_out and elapsed >= 28, (elapsed, result)
                print(f"REPRODUCED: {expected} has no stage deadline ({elapsed:.1f}s operation timeout)")
            else:
                message = json.dumps(result)
                assert result.get("isError") and not timed_out, result
                assert expected in message and "timed out after 15 seconds" in message, result
                assert 14 <= elapsed < 25, (elapsed, result)
                print(f"PASS: {expected} reports stage deadline ({elapsed:.1f}s)")

        self.reset_auth()
        self.use_key(0)
        self.nodes[0].stall_auth = True
        try:
            check("public-key authentication at 127.0.0.1")
        finally:
            self.nodes[0].stall_auth = False
            self.nodes[0].release_stalls.set()
        self.use_key(1)
        self.nodes[1].stall_forward = True
        try:
            check("open jump tunnel inner.invalid:22")
        finally:
            self.nodes[1].stall_forward = False
            self.nodes[1].release_stalls.set()
        # Valid public-key bastions approve forwarding to a peer that accepts
        # TCP but never writes an SSH identification banner.
        silent = socket.socket()
        silent.bind(("127.0.0.1", 0))
        silent.listen()
        original = self.nodes[1].routes[("target.invalid", 22)]
        self.nodes[1].routes[("target.invalid", 22)] = ("127.0.0.1", silent.getsockname()[1])
        try:
            check("SSH handshake to target.invalid:22 via jump")
        finally:
            silent.close()
            self.nodes[1].routes[("target.invalid", 22)] = original
        self.reset_auth()

@contextmanager
def fixture_directory():
    directory = tempfile.TemporaryDirectory(prefix="meatshell-chain-test-")
    try:
        yield directory.name
    finally:
        # Windows scanners can briefly hold the just-exited executable open.
        # Retry only the directory created above; never suppress final failure.
        for attempt in range(50):
            try:
                directory.cleanup()
                break
            except PermissionError:
                if attempt == 49:
                    raise
                time.sleep(0.1)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--expect-old-failure", action="store_true")
    parser.add_argument("--stage-timeouts", action="store_true", help="check stalled network-stage deadlines")
    parser.add_argument("--expect-unbounded-stages", action="store_true", help="reproduce missing deadlines on a pre-fix binary")
    args = parser.parse_args()
    # The fixture is loopback-only; never send its traffic through inherited proxies.
    for name in ("ALL_PROXY", "all_proxy"):
        os.environ.pop(name, None)
    with fixture_directory() as directory:
        fixture = Fixture(args.exe, directory)
        try:
            if args.stage_timeouts or args.expect_unbounded_stages:
                fixture.check_stage_timeouts(args.expect_unbounded_stages)
            else:
                fixture.check(args.expect_old_failure)
        finally:
            for node in fixture.nodes: node.close()
if __name__ == "__main__":
    main()
