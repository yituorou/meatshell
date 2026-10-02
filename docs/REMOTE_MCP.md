# Authenticated remote MCP (Linux or Windows)

`meatshell mcp serve` remains the existing local stdio transport. To run a
remote service, use the **new, opt-in** `--http-config` mode. It serves the
standard MCP Streamable HTTP protocol at `/mcp`, using the official Rust MCP
SDK. Both desktop and `--features headless` builds include this mode. Running
without arguments still starts the GUI in the default desktop build.

This is an **OAuth resource server**, not an identity provider. It verifies
RS256 access tokens from an established external OAuth 2.1 / OIDC provider.
You must configure that provider and an HTTPS reverse proxy before connecting
ChatGPT/dot. No account, token, signing key, tunnel, public listener or production
profile is created by installing the release. Local synthetic integration tests
do not constitute an end-to-end ChatGPT login test.

## Fork update warning

Fork prereleases must be updated manually from the fork's Releases page. The
inherited desktop startup update check and Download banner still point to
`yituorou/meatshell` upstream. The banner opens a web page and does not install
anything automatically, but installing its download can replace fork-only
features. Do not use that upstream banner to update a fork build. You can turn off
“Check for updates on startup” in Settings → Interface. The headless service does
not run the desktop updater. No build-time fork update channel currently exists;
this feature does not silently change upstream defaults or existing user settings.

## 1. Prepare one deliberately selected profile

Run under a dedicated unprivileged OS account. Create a private profile directory
(mode 0700 on Linux) and select it explicitly with `--data-dir` or
`MEATSHELL_DATA_DIR`. HTTP mode refuses to use an implicit default/sidecar profile.
You can import an export using the existing CLI, or configure that profile in
the desktop application. Never place profile files, credentials or SSH keys in
web roots or release packages.

Existing profile settings remain authoritative:

- `mcp_enabled` must be enabled
- Saved credential use requires `mcp_use_saved_credentials`
- Arbitrary SSH commands require `mcp_allow_commands`
- SFTP transfers/imports require `mcp_allow_file_transfers`
- Applying configuration imports additionally requires startup flag
  `--allow-config-import` (before `--http-config`)
- Unknown/changed SSH host keys fail closed. Seed verified host trust as described
  below before using SSH/SFTP through the headless service; never bypass this check

OAuth authorization adds an outer boundary; it does **not** enable these gates.
Authorized subjects all access the same selected profile and the OS user's
filesystem permissions. This is a **single-owner / trusted-operator** service,
not a multi-tenant SSH hosting service. For separate users/data, run separate
OS accounts, profiles and service instances. A subject allowlist is mandatory.

### Seed verified SSH host trust before the first connection

A session export/import **does not include SSH host trust**. A fresh profile is
not ready to connect merely because its sessions and credentials were imported.
The CLI/MCP service cannot approve the desktop host-key dialog: unknown or changed
keys are rejected. Complete this one-time operator step before starting it.

The trust file is `known_hosts` directly inside the explicitly selected private
`--data-dir` (alongside `sessions.json`). It does not read `~/.ssh/known_hosts`.
Use either of these supported approaches:

1. Copy MeatShell's own `known_hosts` from a trusted profile whose server keys you
   previously verified, using a secure local/admin transfer. Select only the
   verified hosts needed by this service. Session host strings and ports must
   still match exactly. A desktop-capable build can establish this trust first:
   select that profile, compare every displayed SHA256 fingerprint against an
   independently trusted server-console/admin record, then approve it. Do not
   approve an unfamiliar key merely to make the connection work.
2. Without a GUI, obtain the server's **public host key** through its trusted
   console or authenticated administrator channel. For example, on that server,
   inspect `/etc/ssh/ssh_host_ed25519_key.pub` and its fingerprint with
   `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256`. Use the actual public
   host key configured by that server's SSH daemon; key paths and types can vary.
   Transfer only the `.pub` key, compare its SHA256 fingerprint against the
   independently verified console/admin value, and add an entry as shown below.
   The server's private host key, your SSH login private key, and an OAuth signing
   key are **never** entries in this file. Unverified `ssh-keyscan` output is not a
   trust source; do not pipe it into the store or blindly accept its fingerprint.

**MeatShell uses its own exact-string format**, not the OpenSSH `known_hosts`
format. Each entry is exactly:

```text
<literal session.host>:<decimal port> <key-type> <base64-public-host-key>
```

Always include the port, including `:22`. Use the precise saved session host
string: a DNS name and its IP address are different entries, and spelling/case is
not normalized. Do not add OpenSSH-style `[host]:port` brackets, hashed hostnames,
comma-separated host lists, wildcard patterns, markers, or a trailing key comment.
For an unbracketed IPv6 session host `2001:db8::10` on port 22, the identifier is
`2001:db8::10:22`. Blank lines and whole-line `#` comments are allowed. The stored
key type and base64 must match the key that the server actually presents.

For example, **after** verifying a single-line public key saved locally as
`verified-bastion-host-key.pub`, run the following as the dedicated service OS
account. Replace the example directory and exact session host/port. Stop the
service/desktop while editing, and preserve a private backup of an existing trust
file before modifying it. These commands append a verified new entry and strip
any `.pub` comment; they do not establish trust or verify identity for you.

```sh
PROFILE=/var/lib/meatshell/profile
umask 077
mkdir -p "$PROFILE"
chmod 700 "$PROFILE"
# Compare the displayed SHA256 fingerprint with the trusted console/admin value.
ssh-keygen -lf verified-bastion-host-key.pub -E sha256
# Continue only after that comparison succeeds and you approve the identity.
touch "$PROFILE/known_hosts"
chmod 600 "$PROFILE/known_hosts"
awk -v id='bastion.example.com:22' \
  'NR == 1 { printf "%s %s %s\n", id, $1, $2 }' \
  verified-bastion-host-key.pub >> "$PROFILE/known_hosts"
```

Keep the profile and file owned by the service account, with directory mode 0700
and file mode 0600 on Linux (equivalent private ACLs on Windows). Restrict backup
copies too. Seed **every jump host and the final target**, each under its own saved
host/port, even when the target is reachable only through a jump. Keep the final
target's saved hostname; do not substitute localhost or the bastion address. Repeat for any
verified host-key type that can be negotiated; a different key is not implicitly
trusted. Start the service only after completing this setup. Unknown/changed keys
must continue to fail closed: investigate a change out of band, then deliberately
replace the old host entry after re-verification. Never erase the store, enable
accept-all behavior, or silently append an unverified replacement to bypass a
failure. No real profile or trust file is supplied in this release.

## 2. Configure the external authorization server

Use an established provider with authorization-code + PKCE S256, issuer
metadata discovery and a ChatGPT-compatible client registration mechanism
(CIMD, DCR, or a predefined client). Configure the exact redirect URI displayed
by the ChatGPT connection setup. Configure it to issue **RS256 access tokens**
with:

- `iss`: exact configured HTTPS issuer, including any trailing slash
- `aud`: exact canonical resource URL, e.g. `https://shell.example.com/mcp`
- `sub`: the explicitly allowed account identifier
- `exp`: a mandatory expiry timestamp; `nbf` is checked when present
- `scope`: a space-separated string containing `meatshell:mcp`

The provider must honor the OAuth `resource` parameter. Do not use ID tokens,
shared static bearer tokens, a client secret, or SSH credentials as access tokens.
Static bearer authentication is not implemented by this HTTP adapter.

Download the provider's **public** JWKS through a trusted, TLS-verified admin
workflow into a local `public-jwks.json` file. The service deliberately does not
fetch URLs supplied by clients. Only RSA/RS256 signature-verification keys with
unique nonempty `kid` values are usable; private/symmetric key material is rejected.
Protect the file against untrusted modification. On rotation, replace it
atomically with the provider's current public keys and restart the service.
For overlap, retain both old and new public keys until old access tokens expire.
Key removal, subject/scope changes and revocation require restarting this service;
JWT revocation/introspection is not supported. Use short token lifetimes.

See the official [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)
and [OpenAI authentication requirements](https://developers.openai.com/plugins/build/auth).

## 3. Create the service configuration

Example `http.json` (all hostnames and subjects below are placeholders):

```json
{
  "bind": "127.0.0.1:8765",
  "allowed_origins": [],
  "oauth": {
    "issuer": "https://identity.example.com/",
    "resource": "https://shell.example.com/mcp",
    "jwks_file": "public-jwks.json",
    "required_scope": "meatshell:mcp",
    "allowed_subjects": ["REPLACE_WITH_YOUR_PROVIDER_SUBJECT"]
  }
}
```

`jwks_file` is relative to the configuration file (or absolute). The default bind
is loopback `127.0.0.1:8765`. Authentication is mandatory for every MCP request,
even loopback requests; there is no unauthenticated/public-bind flag. A non-loopback
bind is supported only for a private, firewalled proxy network; the application
speaks HTTP internally, so do not expose that listener directly to the Internet.

Every request with an `Origin` header must match an explicitly listed HTTPS
origin. With the empty default, all browser-origin requests are rejected; normal
server-to-server MCP requests do not need an Origin. No wildcard CORS is enabled.
Host must match the configured resource authority or bind address. The reverse
proxy must preserve the public Host header. Forwarded headers never grant trust.

Start (example paths are operator-selected, not shipped profiles):

```sh
meatshell --data-dir /var/lib/meatshell/profile mcp serve --http-config /etc/meatshell/http.json
```

No access tokens/passwords belong in command-line arguments or logs. Clients
send access tokens solely using the HTTP `Authorization: Bearer ...` header.

## 4. Terminate HTTPS in a reverse proxy

Example Caddy configuration (install/manage Caddy separately from its official
source; DNS/TLS setup is the operator's responsibility). This proxy configuration
was tested with Caddy 2.11.4 and the published `0.7.4-remote.1` Linux headless
executable, using ephemeral loopback TLS fixtures:

```caddyfile
{
    servers {
        # Allow an early 401/408/413 response without draining a stalled
        # HTTP/1 request body first. Test your clients with this option.
        enable_full_duplex
        timeouts {
            read_header 5s
            read_body 10s
            idle 2m
        }
    }
}
shell.example.com {
    reverse_proxy 127.0.0.1:8765 {
        flush_interval -1
    }
}
```

The `servers` options are global; use a dedicated proxy instance or review their
effect on other sites. Caddy labels `enable_full_duplex` experimental, and older
HTTP/1 clients may not support it. Without it, Caddy's Go HTTP server can wait to
drain an incomplete request body before forwarding MeatShell's early error.
In the loopback test, the application returned a 408 after five seconds, but the
unconfigured proxy did not deliver it within the client's ten-second timeout.
Full duplex preserves that early response; the proxy's own read-header/read-body
deadlines also bound requests which have not reached the application. Adjust
timeouts for your environment and test slow requests, not only successful calls.

Caddy preserves Host and Authorization by default for this HTTP upstream.
`flush_interval -1` flushes streaming responses immediately; it also leaves
backend requests alive if the client disconnects. Use explicit MCP cancellation
or session DELETE to terminate abandoned work, with application deadlines as the
backstop. See Caddy's [server options](https://caddyserver.com/docs/caddyfile/options#enable-full-duplex)
and [reverse-proxy streaming guidance](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#streaming).

Proxy both `/mcp` and `/.well-known/oauth-protected-resource*`. Preserve
Authorization and Host headers, disable buffering of SSE, set suitable request
and idle timeouts, and do not log Authorization or request bodies. Restrict the
upstream HTTP port at the firewall. Configure proxy-level connection/rate limits
and TLS, including protection against slow HTTP headers before requests reach
application middleware.

TLS ends at the proxy in this deployment. The application does not itself accept
HTTPS or gRPC. Keep the cleartext hop on loopback; if the proxy and application
are on different machines, separately protect that hop rather than exposing the
HTTP port. HTTPS already encrypts standard MCP traffic and does not require a
custom gRPC transport.

Optional systemd unit (adjust all paths/user names; this does not install itself):

```ini
[Unit]
Description=MeatShell authenticated MCP
After=network-online.target
[Service]
User=meatshell
Group=meatshell
ExecStart=/opt/meatshell/meatshell --data-dir /var/lib/meatshell/profile mcp serve --http-config /etc/meatshell/http.json
WorkingDirectory=/var/lib/meatshell
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/var/lib/meatshell
Restart=on-failure
TimeoutStopSec=15
[Install]
WantedBy=multi-user.target
```

Filesystem hardening may intentionally prevent upload/download/import outside the
profile. Grant only the paths this service actually needs. Never run as root.
On Windows, use equivalent private directory ACLs and a service manager. Native
Windows packaging/build results are stated separately in each release; a Linux
runtime test is not Windows runtime verification.

## 5. Connect and verify

The public URL is `https://shell.example.com/mcp`. Unauthenticated `/mcp`
requests return 401 with a `WWW-Authenticate` discovery challenge. Public metadata
at `/.well-known/oauth-protected-resource/mcp` (also the root well-known path)
contains only resource/issuer/scope information, never sessions or credentials.

Use MCP Inspector, then [ChatGPT's connection setup](https://developers.openai.com/plugins/deploy/connect-chatgpt).
The external provider, redirect URI, scopes, audience, HTTPS certificate and
public reachability must work together. This release has no preconfigured dot
plugin or production deployment. Do not claim it is connected until that end-to-end
flow succeeds in the target environment.

### Reproduce local HTTP and HTTPS verification

Install Python `cryptography` and `paramiko`, and obtain Caddy from its official
distribution. Point `--exe` at a built binary or an independently downloaded,
checksum-verified release executable:

```sh
python tests/remote_mcp_e2e.py --exe /path/to/meatshell
python tests/remote_mcp_e2e.py --exe /path/to/meatshell --caddy /path/to/caddy
python tests/remote_mcp_e2e.py --exe /path/to/meatshell --caddy /path/to/caddy --protocol-version 2025-11-25
```

The default protocol revision is `2025-06-18`; the optional revision is asserted
against the negotiated initialization result. These tests do not claim support
for the changed `2026-07-28` lifecycle.

HTTPS mode repeats the complete HTTP authentication/session/cancellation suite
through a real Caddy reverse proxy. It generates a one-hour test CA and localhost
certificate at runtime, trusts the CA only in that Python client's SSL context,
and verifies TLS 1.2/1.3 plus rejection of an untrusted CA and wrong hostname.
The external HTTPS resource audience and discovery URL must survive proxying;
wrong issuer/audience/signature/expiry/scope/subject, disallowed Host/Origin,
oversized and stalled bodies, permission gates, explicit cancellation, token
expiry, session deletion and active SFTP-upload cancellation are exercised.
Both listeners are loopback-only. Caddy's admin API, automatic certificate
issuance and HTTP/3 listener are disabled in the fixture. No system CA store,
real profile, public tunnel or production account is touched. Certificates,
private keys and fixture state are generated under temporary storage and are
never committed or packaged.

This proves the released executable works behind locally verified HTTPS. It
does **not** prove public DNS/ingress, a publicly trusted certificate, external
OAuth authorization-code/PKCE login, or a ChatGPT/dot connection. Verify those
separately in the intended deployment before calling the service connected.

## Runtime limits and cancellation

- RS256 signature, issuer, audience, expiry/not-before, scope and subject checked
  on every request; invalid tokens return 401, insufficient permission 403
- MCP session IDs bind to the authenticated subject of the configured issuer;
  IDs belonging to another subject/unknown/expired IDs all return 404
- At most 64 protocol sessions, 15-minute absolute lifetime, SDK idle cleanup;
  clients must reinitialize after expiry. Refreshing an OAuth token for the same
  subject can continue a live session; subject changes cannot
- 1 MiB HTTP body limit, 5-second body-read deadline, 10-second protocol-header
  response deadline, 16 tool calls/response streams; control notifications retain
  separate admission capacity
- Tool calls and response streams end after at most 300 seconds or token expiry,
  whichever occurs first. Existing per-operation limits still apply
- Use `notifications/cancelled` to cancel an in-flight request, and HTTP DELETE to
  close its MCP session. Cancellation stops local SSH/SFTP work; it cannot undo
  commands already executed, bytes already written, or remote jobs detached by a
  command. A dropped network connection is not proof an action was undone
- Results use POST SSE response streams. Standalone GET streams/resumption are
  deliberately unsupported (405); completed-response replay caching is disabled
- SIGTERM/Ctrl-C requests graceful shutdown. Configuration/keys are loaded once
  per process; restart after changes

## Reproducible checks

```sh
cargo test --features headless --bin meatshell
cargo build --features headless
python3 tests/remote_mcp_e2e.py --exe target/debug/meatshell
python3 tests/config_import_e2e.py --exe target/debug/meatshell
# Synthetic HTTP and SSH/SFTP tests require Python cryptography + paramiko:
python3 tests/ssh_jump_chain_e2e.py --exe target/debug/meatshell
python3 tests/ssh_jump_chain_e2e.py --exe target/debug/meatshell --stage-timeouts
cargo check                         # default desktop source compatibility
```

Tests generate synthetic RSA signing keys in memory, write only their public
JWKS, create disposable profiles and connect solely to loopback fixtures.
A headless archive contains CLI/MCP functionality and no GUI; desktop archives
keep the GUI, CLI, stdio MCP and HTTP MCP in the same executable.
