"""Loopback SFTP transfer regression for the concurrent folder-batch rework.

Run: python tests/sftp_folder_e2e.py --exe target/debug/meatshell
No external servers or real credentials: a paramiko loopback SSH/SFTP server
serves a throwaway local directory on 127.0.0.1.

Folder batches themselves are a GUI-only path: CLI/MCP uploads reject
directories and the automation adapter returns on the first terminal transfer
event, so neither can drive a whole-folder batch. This script therefore guards
the shared pipelined upload/download implementations that the batch scheduler
reuses (their `already_queued = false` call sites), via single-file
round-trips. Batch enumeration, queue registration, failure isolation and
cancellation are covered by Rust unit tests in src/sftp/impls/sftp.rs.
"""
import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
import threading
import paramiko

PASSWORD = "loopback-fixture-only"


class LocalSFTP(paramiko.SFTPServerInterface):
    """Serve a real local directory over SFTP so writes land on disk."""

    def __init__(self, server, root):
        super().__init__(server)
        self.root = pathlib.Path(root)

    def _local(self, path):
        return self.root / path.lstrip("/")

    def _attrs(self, path):
        st = self._local(path).stat()
        attrs = paramiko.SFTPAttributes.from_stat(st)
        attrs.filename = pathlib.PurePosixPath(path).name
        return attrs

    def list_folder(self, path):
        try:
            return [self._attrs(str(pathlib.PurePosixPath(path) / child.name))
                    for child in self._local(path).iterdir()]
        except OSError as exc:
            return exc.errno

    def stat(self, path):
        try:
            return self._attrs(path)
        except OSError as exc:
            return exc.errno

    lstat = stat

    def mkdir(self, path, attr):
        try:
            self._local(path).mkdir(parents=False, exist_ok=False)
            return paramiko.SFTP_OK
        except OSError as exc:
            return exc.errno

    def rmdir(self, path):
        try:
            self._local(path).rmdir()
            return paramiko.SFTP_OK
        except OSError as exc:
            return exc.errno

    def remove(self, path):
        try:
            self._local(path).unlink()
            return paramiko.SFTP_OK
        except OSError as exc:
            return exc.errno

    def rename(self, old_path, new_path):
        try:
            shutil.move(str(self._local(old_path)), str(self._local(new_path)))
            return paramiko.SFTP_OK
        except OSError as exc:
            return exc.errno

    @staticmethod
    def _open_mode(flags):
        if flags & os.O_APPEND:
            return "ab"
        if flags & (os.O_WRONLY | os.O_RDWR):
            if flags & (os.O_CREAT | os.O_TRUNC):
                return "wb"
            return "r+b"
        return "rb"

    def open(self, path, flags, attr):
        try:
            file_obj = self._local(path).open(self._open_mode(flags))
        except OSError as exc:
            return exc.errno
        handle = paramiko.SFTPHandle(flags)
        handle.readfile = file_obj
        handle.writefile = file_obj
        return handle


class PasswordServer(paramiko.ServerInterface):
    def get_allowed_auths(self, username):
        return "password"

    def check_auth_password(self, username, password):
        if username == "fixture" and password == PASSWORD:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind, chanid):
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED


class LoopbackSftp:
    def __init__(self, root):
        self.key = paramiko.RSAKey.generate(2048)
        self.root = pathlib.Path(root)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        self._thread = threading.Thread(target=self._accept, daemon=True)
        self._thread.start()

    def _accept(self):
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn):
        transport = paramiko.Transport(conn)
        try:
            transport.add_server_key(self.key)
            transport.set_subsystem_handler(
                "sftp", paramiko.SFTPServer, LocalSFTP, str(self.root)
            )
            transport.start_server(server=PasswordServer())
            # Never call transport.accept() here: popping the session channel
            # out of the accept queue and dropping the only reference makes
            # CPython garbage-collect it before the client's subsystem request
            # arrives, and Channel.__del__ closes the channel (EOF + CLOSE),
            # so the SFTP handshake fails with "channel closed". The SFTP
            # subsystem is driven by paramiko's transport thread itself; just
            # wait for that thread to finish when the client disconnects.
            transport.join()
        except (EOFError, OSError, paramiko.SSHException):
            pass
        finally:
            transport.close()

    def close(self):
        self.listener.close()


class MeatShellCli:
    def __init__(self, exe, directory, server):
        self.exe = pathlib.Path(exe)
        self.profile = pathlib.Path(directory) / "profile"
        self.profile.mkdir()
        sessions = [
            dict(id="target", name="target", host="127.0.0.1", port=server.port,
                 user="fixture", auth="password", password=PASSWORD, kind="ssh")
        ]
        (self.profile / "sessions.json").write_text(
            json.dumps(dict(
                sessions=sessions, defaults_rev=3,
                mcp_enabled=True, mcp_use_saved_credentials=True,
                mcp_allow_commands=True, mcp_allow_file_transfers=True,
            )),
            encoding="utf-8",
        )
        (self.profile / "known_hosts").write_text(
            f"127.0.0.1:{server.port} {server.key.get_name()} {server.key.get_base64()}\n",
            encoding="utf-8",
        )

    def run(self, *args):
        return subprocess.run(
            [str(self.exe), "--data-dir", str(self.profile), "cli", *args],
            capture_output=True, text=True, encoding="utf-8", timeout=60,
        )

    def upload(self, local, remote_dir):
        return self.run("upload", "target", str(local), remote_dir, "--json")

    def download(self, remote, local_dir):
        return self.run("download", "target", remote, str(local_dir), "--json")


def expect_ok(result, label):
    assert result.returncode == 0, f"{label} failed: {result.stderr or result.stdout}"
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=pathlib.Path)
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="meatshell-sftp-e2e-") as tmp:
        tmp = pathlib.Path(tmp)
        remote_root = tmp / "remote"
        remote_root.mkdir()
        server = LoopbackSftp(remote_root)
        try:
            cli = MeatShellCli(args.exe, tmp, server)

            # 1. Upload: the GUI batch scheduler reuses this exact pipeline.
            staging = tmp / "staging"
            staging.mkdir()
            # Real SFTP servers don't create missing parent directories, so the
            # upload target must already exist (same expectation as the GUI).
            uploads = remote_root / "uploads"
            uploads.mkdir()
            payload = "文件夹批次并发改造回归\n" * 128
            sample = staging / "sample.txt"
            sample.write_text(payload, encoding="utf-8")
            value = expect_ok(cli.upload(sample, "/uploads"), "upload")
            assert value["direction"] == "upload", value
            landed = uploads / "sample.txt"
            assert landed.read_text(encoding="utf-8") == payload, "uploaded bytes mismatch"
            print("PASS: single-file upload lands intact through the shared pipeline")

            # 2. Download a top-level file and a nested one (path handling).
            tree = remote_root / "tree"
            (tree / "sub").mkdir(parents=True)
            (tree / "a.txt").write_bytes(b"top-level\n")
            (tree / "sub" / "b.txt").write_bytes(b"nested\n")
            downloads = tmp / "downloads"
            downloads.mkdir()
            value = expect_ok(cli.download("/tree/a.txt", downloads), "download top-level")
            assert value["direction"] == "download", value
            assert (downloads / "a.txt").read_bytes() == b"top-level\n"
            nested_dest = tmp / "nested-download"
            nested_dest.mkdir()
            expect_ok(cli.download("/tree/sub/b.txt", nested_dest), "download nested")
            assert (nested_dest / "b.txt").read_bytes() == b"nested\n"
            print("PASS: single-file downloads handle top-level and nested paths")

            # 3. Round-trip: download what a previous upload created.
            roundtrip = tmp / "roundtrip"
            roundtrip.mkdir()
            expect_ok(cli.download("/uploads/sample.txt", roundtrip), "round-trip download")
            assert (roundtrip / "sample.txt").read_text(encoding="utf-8") == payload
            print("PASS: uploaded file downloads back byte-identical")
        finally:
            server.close()
    print("All SFTP transfer regression checks passed.")


if __name__ == "__main__":
    main()
