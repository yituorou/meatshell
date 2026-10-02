"""Test-only Caddy TLS fixture. Never installs a CA or changes system trust.

All certificates, keys, profiles, listeners and proxy state are ephemeral.
The caller supplies Caddy from an official installation; this module does not
download software, request public certificates, or expose a public listener.
"""
import datetime
import http.client
import socket
import ssl
import subprocess
import time

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID


class CaddyTlsFixture:
    def __init__(self, executable, root, upstream_port, child_env):
        self.root = root / 'tls-proxy'
        self.root.mkdir(mode=0o700)
        self.process = None
        self.executable = executable
        self.env = dict(child_env, HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / 'config'),
                        XDG_DATA_HOME=str(self.root / 'data'))
        self.log = self.root / 'caddy.log'
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            self.port = reserve.getsockname()[1]
        self.resource = f'https://localhost:{self.port}/mcp'

        now = datetime.datetime.now(datetime.timezone.utc)
        ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'MeatShell ephemeral test CA')])
        ca = (x509.CertificateBuilder().subject_name(ca_name).issuer_name(ca_name)
              .public_key(ca_key.public_key()).serial_number(x509.random_serial_number())
              .not_valid_before(now - datetime.timedelta(minutes=1))
              .not_valid_after(now + datetime.timedelta(hours=1))
              .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
              .add_extension(x509.KeyUsage(digital_signature=False, content_commitment=False,
                  key_encipherment=False, data_encipherment=False, key_agreement=False,
                  key_cert_sign=True, crl_sign=True, encipher_only=False, decipher_only=False), critical=True)
              .add_extension(x509.SubjectKeyIdentifier.from_public_key(ca_key.public_key()), critical=False)
              .sign(ca_key, hashes.SHA256()))
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        leaf = (x509.CertificateBuilder()
                .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')]))
                .issuer_name(ca_name).public_key(key.public_key()).serial_number(x509.random_serial_number())
                .not_valid_before(now - datetime.timedelta(minutes=1))
                .not_valid_after(now + datetime.timedelta(hours=1))
                .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost')]), critical=False)
                .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
                .add_extension(x509.KeyUsage(digital_signature=True, content_commitment=False,
                    key_encipherment=True, data_encipherment=False, key_agreement=False,
                    key_cert_sign=False, crl_sign=False, encipher_only=False, decipher_only=False), critical=True)
                .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()), critical=False)
                .sign(ca_key, hashes.SHA256()))
        self.ca_file = self.root / 'ca.pem'
        self.ca_file.write_bytes(ca.public_bytes(serialization.Encoding.PEM))
        certificate = self.root / 'server.pem'
        certificate.write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
        private_key = self.root / 'server-key.pem'
        private_key.write_bytes(key.private_bytes(serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
        private_key.chmod(0o600)
        self.context = ssl.create_default_context(cafile=str(self.ca_file))
        self.config = self.root / 'Caddyfile'
        self.config.write_text('''{
    admin off
    auto_https off
    default_sni localhost
    grace_period 2s
    servers {
        protocols h1 h2
        enable_full_duplex
        timeouts {
            read_header 5s
            read_body 10s
            idle 2m
        }
    }
}
https://:%d {
    bind 127.0.0.1
    tls "%s" "%s"
    reverse_proxy 127.0.0.1:%d {
        flush_interval -1
    }
}
''' % (self.port, certificate.as_posix(), private_key.as_posix(), upstream_port))

    def __enter__(self):
        with self.log.open('wb') as output:
            self.process = subprocess.Popen([str(self.executable), 'run', '--config', str(self.config),
                '--adapter', 'caddyfile'], stdout=output, stderr=output, env=self.env)
        try:
            for _ in range(100):
                if self.process.poll() is not None:
                    raise AssertionError('Caddy exited: ' + self.log.read_text())
                try:
                    client = self.connection(timeout=.1)
                    client.connect()
                    client.close()
                    return self
                except OSError:
                    time.sleep(.1)
            raise AssertionError('Caddy TLS listener not ready: ' + self.log.read_text())
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def connection(self, timeout):
        return http.client.HTTPSConnection('localhost', self.port, context=self.context, timeout=timeout)

    def check_certificate_validation(self):
        for host, context in [('localhost', ssl.create_default_context()), ('127.0.0.1', self.context)]:
            client = http.client.HTTPSConnection(host, self.port, context=context, timeout=3)
            try:
                client.connect()
            except ssl.SSLCertVerificationError:
                pass
            else:
                raise AssertionError('TLS accepted an untrusted issuer or incorrect hostname')
            finally:
                client.close()
        for version in (ssl.TLSVersion.TLSv1_2, ssl.TLSVersion.TLSv1_3):
            context = ssl.create_default_context(cafile=str(self.ca_file))
            context.minimum_version = context.maximum_version = version
            with socket.create_connection(('127.0.0.1', self.port), timeout=3) as raw:
                with context.wrap_socket(raw, server_hostname='localhost') as tls:
                    assert tls.version() in ('TLSv1.2', 'TLSv1.3')
        print('PASS: real Caddy TLS 1.2/1.3; untrusted CA and wrong hostname rejected; client-only CA trust')

    def __exit__(self, error_type, *_):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
                if error_type is None:
                    raise AssertionError('Caddy shutdown exceeded five seconds')
