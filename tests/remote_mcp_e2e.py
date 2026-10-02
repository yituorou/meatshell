"""Authenticated MCP regression tests. Only ephemeral keys and loopback fixtures.

Requires Python cryptography and paramiko; run with --exe <headless-or-desktop-meatshell>.
Add --caddy <caddy-executable> to repeat the suite through verified loopback HTTPS.
No production OAuth server, SSH server, profile or credential is contacted.
"""
import argparse
import base64
import concurrent.futures
import contextlib
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from cryptography.hazmat.primitives.asymmetric import rsa, padding
from cryptography.hazmat.primitives import hashes


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b'=').decode()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', required=True, type=Path)
    parser.add_argument('--caddy', type=Path, help='test through a loopback-only TLS reverse proxy')
    parser.add_argument('--protocol-version', choices=['2025-06-18', '2025-11-25'], default='2025-06-18')
    args = parser.parse_args()
    exe = args.exe.resolve()
    # Synthetic loopback fixtures must not use the caller's outbound proxy.
    child_env = {k:v for k,v in os.environ.items() if k.lower() not in ('all_proxy', 'http_proxy', 'https_proxy')}

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    public = key.public_key().public_numbers()
    issuer = 'https://issuer.example.invalid'
    resource = 'https://meatshell.example.invalid/mcp'

    def token(**updates):
        claims = dict(iss=issuer, aud=resource, sub='alice', exp=int(time.time()) + 600,
                      scope='meatshell:mcp', nbf=int(time.time()) - 1)
        claims.update(updates)
        for k in [k for k, v in claims.items() if v is None]:
            del claims[k]
        payload = b64(json.dumps(dict(alg='RS256', kid='fixture')).encode()) + '.' + b64(json.dumps(claims).encode())
        return payload + '.' + b64(key.sign(payload.encode(), padding.PKCS1v15(), hashes.SHA256()))

    with tempfile.TemporaryDirectory(prefix='meatshell-http-fixture-') as temp, contextlib.ExitStack() as stack:
        root = Path(temp)
        profile = root / 'profile'
        profile.mkdir(mode=0o700)
        saved = profile / 'sessions.json'
        config = dict(defaults_rev=3, mcp_enabled=True, mcp_use_saved_credentials=True,
                      mcp_allow_commands=False, mcp_allow_file_transfers=False,
                      sessions=[dict(id='fixture', name='Fixture', host='127.0.0.1', port=1,
                                     user='fixture', auth='password', password='synthetic-not-a-real-secret', kind='ssh')])
        saved.write_text(json.dumps(config))
        jwks = root / 'public-jwks.json'
        jwks.write_text(json.dumps({'keys': [dict(kty='RSA', kid='fixture', alg='RS256', use='sig',
            n=b64(public.n.to_bytes((public.n.bit_length()+7)//8, 'big')),
            e=b64(public.e.to_bytes((public.e.bit_length()+7)//8, 'big')))]}))
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        def connection(timeout):
            return http.client.HTTPConnection('127.0.0.1', port, timeout=timeout)
        if args.caddy:
            from tls_proxy_fixture import CaddyTlsFixture
            proxy = stack.enter_context(CaddyTlsFixture(args.caddy.resolve(), root, port, child_env))
            resource = proxy.resource
            connection = proxy.connection
            proxy.check_certificate_validation()
        http_config = root / 'http.json'
        http_settings = dict(bind=f'127.0.0.1:{port}', allowed_origins=['https://client.example.invalid'],
            oauth=dict(issuer=issuer, resource=resource, jwks_file=str(jwks),
                       required_scope='meatshell:mcp', allowed_subjects=['alice', 'bob']))
        http_config.write_text(json.dumps(http_settings))
        # Fail closed at startup: no implicit profile, no insecure resource,
        # no open subject policy, and no private signing material in JWKS.
        start = [str(exe), '--data-dir', str(profile), 'mcp', 'serve', '--http-config', str(http_config)]
        no_profile = subprocess.run([str(exe), 'mcp', 'serve', '--http-config', str(http_config)],
            env={k:v for k,v in os.environ.items() if k != 'MEATSHELL_DATA_DIR'}, capture_output=True, timeout=10)
        assert no_profile.returncode != 0
        for field, invalid in [('issuer', 'http://issuer.example.invalid'), ('resource', 'http://shell.invalid/mcp'), ('allowed_subjects', [])]:
            original = http_settings['oauth'][field]
            http_settings['oauth'][field] = invalid
            http_config.write_text(json.dumps(http_settings))
            assert subprocess.run(start, capture_output=True, timeout=10).returncode != 0
            http_settings['oauth'][field] = original
        http_config.write_text(json.dumps(http_settings))
        public_text = jwks.read_text()
        private_example = json.loads(public_text)
        private_example['keys'][0]['d'] = 'synthetic-invalid-private-component'
        jwks.write_text(json.dumps(private_example))
        assert subprocess.run(start, capture_output=True, timeout=10).returncode != 0
        jwks.write_text(public_text)
        print('PASS: explicit profile, HTTPS issuer/resource, subject allowlist and public-only JWKS startup checks')
        log = root / 'stderr.log'
        with log.open('wb') as output:
            process = subprocess.Popen([str(exe), '--data-dir', str(profile), 'mcp', 'serve', '--http-config', str(http_config)],
                                       stdout=subprocess.DEVNULL, stderr=output, env=child_env)
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError('server exited: ' + log.read_text())
                try:
                    with socket.create_connection(('127.0.0.1', port), .1):
                        break
                except OSError:
                    time.sleep(.1)
            else:
                raise AssertionError('server not ready')

            def request(payload=None, auth=None, session=None, method='POST', path='/mcp', headers=None, raw=None):
                client = connection(timeout=15)
                hdr = {'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream',
                       'MCP-Protocol-Version': args.protocol_version}
                if auth is not None:
                    hdr['Authorization'] = 'Bearer ' + auth
                if session is not None:
                    hdr['Mcp-Session-Id'] = session
                hdr.update(headers or {})
                body = raw if raw is not None else (json.dumps(payload) if payload is not None else None)
                client.request(method, path, body=body, headers=hdr)
                response = client.getresponse()
                content = response.read().decode()
                status, response_headers = response.status, dict(response.getheaders())
                client.close()
                value = None
                if content.strip():
                    if content.lstrip().startswith('{'):
                        value = json.loads(content)
                    else:
                        for line in content.splitlines():
                            if line.startswith('data:') and line[5:].strip():
                                candidate = json.loads(line[5:])
                                if isinstance(candidate, dict) and 'jsonrpc' in candidate:
                                    value = candidate
                return status, {k.lower(): v for k, v in response_headers.items()}, value

            init = dict(jsonrpc='2.0', id=1, method='initialize', params=dict(protocolVersion=args.protocol_version,
                capabilities={}, clientInfo=dict(name='synthetic-regression', version='1')))
            alice, bob = token(), token(sub='bob')
            status, headers, _ = request(init)
            assert status == 401 and 'resource_metadata=' in headers['www-authenticate']
            metadata_url = resource.rsplit('/', 1)[0] + '/.well-known/oauth-protected-resource/mcp'
            assert f'resource_metadata="{metadata_url}"' in headers['www-authenticate']
            for path in ['/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp']:
                status, _, value = request(method='GET', path=path)
                assert status == 200 and value['resource'] == resource
                assert value['authorization_servers'] == [issuer] and 'sessions' not in json.dumps(value)
            for invalid in ['bad', alice[:-5]+'AAAAA', token(exp=int(time.time())-1), token(aud='https://other.invalid/mcp'),
                            token(iss='https://other.invalid'), token(exp=None), token(aud=None), token(nbf=int(time.time())+600)]:
                assert request(init, invalid)[0] == 401
            for denied in [token(sub='mallory'), token(scope='other'), token(scope=None)]:
                assert request(init, denied)[0] == 403
            assert request(init, alice, headers={'Origin': 'https://evil.invalid'})[0] == 403
            assert request(init, alice, headers={'Host': 'evil.invalid'})[0] == 403
            assert request(init, alice, headers={'Origin': 'https://client.example.invalid'})[0] == 200
            print('PASS: metadata, signature/expiry/issuer/audience/scope/subject/nbf, origin and Host checks')

            def initialize(auth):
                status, hdr, value = request(init, auth)
                assert status == 200 and value['result']['serverInfo']['name'] == 'meatshell', (status, value)
                assert value['result']['protocolVersion'] == args.protocol_version
                session = hdr['mcp-session-id']
                assert request(dict(jsonrpc='2.0', method='notifications/initialized'), auth, session)[0] == 202
                return session

            a, b = initialize(alice), initialize(bob)
            assert a != b
            listing = dict(jsonrpc='2.0', id=2, method='tools/list')
            assert request(listing, bob, a)[0] == 404
            assert request(listing, alice, b)[0] == 404
            assert request(listing, alice, 'not-a-session')[0] == 404
            assert request(listing, token(sub='mallory'), a)[0] == 403
            assert request(listing, token(exp=int(time.time())-1), a)[0] == 401
            status, _, listed = request(listing, alice, a)
            assert status == 200 and len(listed['result']['tools']) == 8
            assert all(t.get('securitySchemes') == t.get('_meta', {}).get('securitySchemes') and t['securitySchemes'][0]['type'] == 'oauth2' for t in listed['result']['tools'])
            assert request(method='GET', auth=alice, session=a)[0] == 405
            print('PASS: authenticated initialize/list, OAuth tool metadata and principal session isolation')

            def call(name, args, request_id=3, session=a):
                return request(dict(jsonrpc='2.0', id=request_id, method='tools/call', params=dict(name=name, arguments=args)), alice, session)

            status, _, value = call('list_sessions', {})
            assert status == 200 and not value['result'].get('isError')
            text = json.dumps(value)
            assert 'synthetic-not-a-real-secret' not in text and 'Fixture' in text
            _, _, value = call('run_command', dict(session_id='fixture', command='whoami'))
            assert value['result']['isError'] and 'disabled' in json.dumps(value).lower()
            _, _, value = call('upload_file', dict(session_id='fixture', local_path=str(jwks), remote_directory='/tmp'))
            assert value['result']['isError']
            export = root / 'export.json'
            export.write_text(json.dumps({'sessions': []}))
            config['mcp_allow_file_transfers'] = True
            saved.write_text(json.dumps(config))
            _, _, value = call('import_sessions', dict(local_path=str(export), dry_run=False))
            assert value['result']['isError'] and '--allow-config-import' in json.dumps(value)
            config['mcp_use_saved_credentials'] = False
            config['mcp_allow_commands'] = True
            saved.write_text(json.dumps(config))
            _, _, value = call('run_command', dict(session_id='fixture', command='whoami'))
            assert value['result']['isError'] and 'saved' in json.dumps(value).lower()
            print('PASS: redacted read tool; command, file transfer, import and saved-credential gates')

            assert request(auth=alice, session=a, raw='x'*(1024*1024+1))[0] == 413
            assert request(auth=alice, session=a, raw='{bad')[0] == 400
            assert request(listing, alice)[0] == 400
            slow = connection(timeout=10)
            slow.putrequest('POST', '/mcp')
            for k,v in {'Authorization': 'Bearer '+alice, 'Content-Type': 'application/json',
                        'Accept': 'application/json, text/event-stream', 'Mcp-Session-Id': a,
                        'Content-Length': '1000'}.items():
                slow.putheader(k,v)
            slow.endheaders()
            slow.send(b'{')
            started = time.monotonic()
            slow_response = slow.getresponse()
            assert slow_response.status == 408 and 4 <= time.monotonic()-started < 8
            slow_response.read()
            slow.close()
            print('PASS: body cap, slow-body deadline, malformed JSON, initialization requirement')

            # A silent synthetic SSH peer proves cancellation drops network work.
            peer = socket.socket()
            peer.bind(('127.0.0.1', 0))
            peer.listen()
            peer.settimeout(10)
            config['sessions'][0]['port'] = peer.getsockname()[1]
            config['mcp_use_saved_credentials'] = True
            saved.write_text(json.dumps(config))
            accepted = threading.Event()
            closed = threading.Event()
            def silent_peer():
                connection, _ = peer.accept()
                accepted.set()
                connection.settimeout(8)
                try:
                    while connection.recv(8192):
                        pass
                    closed.set()
                except ConnectionResetError:
                    closed.set()
                finally:
                    connection.close()
            thread = threading.Thread(target=silent_peer, daemon=True)
            thread.start()
            with concurrent.futures.ThreadPoolExecutor() as pool:
                future = pool.submit(call, 'run_command', dict(session_id='fixture', command='true', timeout_seconds=60), 77)
                assert accepted.wait(5), ('SSH fixture never reached', future.result(timeout=1) if future.done() else 'request still pending')
                started = time.monotonic()
                assert request(dict(jsonrpc='2.0', method='notifications/cancelled', params={'requestId': 77}), alice, a)[0] == 202
                result = future.result(timeout=5)
                assert time.monotonic()-started < 5
                assert closed.wait(5), 'cancelled SSH transport did not close'
            print('PASS: MCP cancellation promptly closes synthetic stalled SSH transport')
            accepted.clear()
            closed.clear()
            threading.Thread(target=silent_peer, daemon=True).start()
            short_token = token(exp=int(time.time()) + 2)
            started = time.monotonic()
            request(dict(jsonrpc='2.0', id=79, method='tools/call', params=dict(name='run_command',
                arguments=dict(session_id='fixture', command='true', timeout_seconds=60))), short_token, a)
            assert accepted.is_set() and closed.wait(3), 'expired access token did not close in-flight SSH'
            assert time.monotonic() - started < 4
            print('PASS: token expiry cancels an in-flight operation and bounds its result stream')
            accepted.clear()
            closed.clear()
            threading.Thread(target=silent_peer, daemon=True).start()
            disposable = initialize(alice)
            disconnected = connection(timeout=10)
            disconnected.request('POST', '/mcp', body=json.dumps(dict(jsonrpc='2.0', id=80, method='tools/call',
                params=dict(name='run_command', arguments=dict(session_id='fixture', command='true', timeout_seconds=60)))),
                headers={'Authorization': 'Bearer '+alice, 'Content-Type': 'application/json',
                         'Accept': 'application/json, text/event-stream', 'Mcp-Session-Id': disposable,
                         'MCP-Protocol-Version': args.protocol_version})
            pending_response = disconnected.getresponse()
            assert accepted.wait(5)
            pending_response.close()
            disconnected.close()
            assert request(auth=alice, session=disposable, method='DELETE')[0] in (200,202)
            assert closed.wait(5), 'DELETE after network disconnect did not close SSH'
            assert request(listing, alice, disposable)[0] == 404
            peer.close()
            print('PASS: disconnected request can be explicitly terminated with session DELETE')

            # Reuse the existing synthetic SSH fixture, with a deliberately slow
            # in-memory upload endpoint. Cancelling must stop detached transfers.
            import paramiko
            import ssh_jump_chain_e2e as ssh_fixture
            received = threading.Event()
            writes = []
            class SlowHandle(paramiko.SFTPHandle):
                def write(self, offset, data):
                    writes.append(len(data))
                    received.set()
                    time.sleep(.02)
                    return paramiko.SFTP_OK
            class SlowFiles(ssh_fixture.Files):
                def open(self, path, flags, attr):
                    return SlowHandle(flags)
            original_files = ssh_fixture.Files
            ssh_fixture.Files = SlowFiles
            node = ssh_fixture.Node('http-transfer')
            try:
                config['sessions'][0].update(port=node.port, password=ssh_fixture.PASSWORD)
                saved.write_text(json.dumps(config))
                (profile / 'known_hosts').write_text(f'127.0.0.1:{node.port} {node.key.get_name()} {node.key.get_base64()}\n')
                upload = root / 'synthetic-upload.bin'
                upload.write_bytes(b'x' * (8 * 1024 * 1024))
                with concurrent.futures.ThreadPoolExecutor() as pool:
                    future = pool.submit(call, 'upload_file', dict(session_id='fixture', local_path=str(upload),
                        remote_directory='/tmp', timeout_seconds=60), 78)
                    assert received.wait(8), 'SFTP upload fixture never received bytes'
                    assert request(dict(jsonrpc='2.0', method='notifications/cancelled', params={'requestId': 78}), alice, a)[0] == 202
                    future.result(timeout=5)
                    deadline = time.monotonic() + 5
                    while any(t.is_active() for t in node.transports) and time.monotonic() < deadline:
                        time.sleep(.05)
                    assert not any(t.is_active() for t in node.transports), 'cancelled SFTP SSH transport is still active'
                    # The server can finish packets already buffered before
                    # disconnect; cancellation cannot roll back remote writes.
                    deadline = time.monotonic() + 4
                    count = len(writes)
                    while time.monotonic() < deadline:
                        time.sleep(.2)
                        if len(writes) == count:
                            break
                        count = len(writes)
                    else:
                        raise AssertionError('SFTP fixture did not drain after transport cancellation')
                    assert sum(writes) < upload.stat().st_size
                print('PASS: cancellation during active upload aborts child transfer and SSH transport')
            finally:
                node.close()
                ssh_fixture.Files = original_files

            assert request(auth=alice, session=a, method='DELETE')[0] in (200, 202)
            assert request(listing, alice, a)[0] == 404
            assert request(listing, bob, b)[0] == 200
            print('PASS: session DELETE invalidates only its owner session')
        finally:
            process.terminate()
            try:
                process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise AssertionError('service shutdown exceeded 12 seconds')
            output = log.read_text()
            if args.caddy:
                output += proxy.log.read_text()
            assert locals().get('alice', 'not-a-log-value') not in output and locals().get('bob', 'not-a-log-value') not in output
            assert 'synthetic-not-a-real-secret' not in output
        print(f'PASS: clean shutdown; no token/credential logging; negotiated MCP {args.protocol_version}')


if __name__ == '__main__':
    main()
