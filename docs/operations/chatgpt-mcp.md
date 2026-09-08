# ChatGPT MCP and the outbound-only bridge

## What v0.1 provides

`cortex-mcp-gateway` is a local Streamable HTTP MCP adapter. It verifies an
OIDC bearer token, maps an already paired subject to a daemon principal, and
forwards through that principal's protected IPC enrollment. The final
capability/policy decision remains in `cortexd`.

The gateway binds its only local HTTP listener to `127.0.0.1`. It then opens an
**outbound-only** mutually authenticated TLS connection to a configured,
stateless relay. It never binds a public listener, and the relay must not have
SQLite access, an enrollment private key, model credentials, or authorization
responsibility.

## Required provisioning boundary

The release includes no `brain pair`, `cortexd enroll-remote`, or automatic
ChatGPT-account enrollment command. Creating a remote principal and its
protected IPC enrollment is a trusted deployment/provisioning operation, not a
request that an unauthenticated client may make. Consequently, Cortex cannot
truthfully claim that a live ChatGPT connection has been configured merely by
starting the gateway.

Before enabling a remote client, an operator must provide all of these:

1. A relay that implements the v0.1 registration/forwarding contract and
   presents a certificate trusted by the gateway.
2. A client certificate/key pair for mTLS, stored in protected local files.
3. An OIDC issuer, audience, permitted signing algorithms, and a stable subject
   for the remote identity.
4. One durable Cortex principal with explicit grants and one matching protected
   IPC enrollment file.
5. A ChatGPT MCP connector configuration supported by the operator's current
   ChatGPT plan that can authenticate with the paired OIDC identity and route
   via the relay. Verify that external connector's current requirements with its
   provider; this repository does not implement or configure the ChatGPT UI.

## Gateway configuration shape

Set `CORTEX_GATEWAY_CONFIG` to the path of a JSON file, then run the gateway:

```powershell
$env:CORTEX_GATEWAY_CONFIG = 'C:\CortexData\gateway.json'
cargo run -p cortex-mcp-gateway
```

The following is a **shape-only** configuration. Replace every angle-bracketed
item through the trusted provisioning boundary; it is not runnable as pasted
and contains no credential value. Unknown fields, including any public-listen
field, are rejected.

```json
{
  "local_port": 0,
  "oidc": {
    "issuer": "https://<issuer-host>",
    "audience": "<audience>",
    "algorithms": ["EdDSA"],
    "cache_ttl_seconds": 300
  },
  "paired_subjects": [
    {
      "subject": "<stable-oidc-subject>",
      "principal_id": "<paired-principal-uuidv7>",
      "ipc_enrollment_path": "C:\\CortexData\\<protected-enrollment-file>"
    }
  ],
  "relay": {
    "host": "<relay-host>",
    "port": 443,
    "server_name": "<relay-tls-server-name>",
    "route_id": "<opaque-route-id>",
    "public_host": "<relay-public-host>",
    "client_certificate_path": "C:\\CortexData\\<client-certificate-pem>",
    "client_private_key_path": "C:\\CortexData\\<protected-client-private-key-pem>"
  }
}
```

`local_port` may be `0` to request an ephemeral loopback port. The gateway
still binds only to `127.0.0.1`; the relay connection is the sole remote
transport. The OIDC cache is bounded, refreshes an unknown key once, and uses a
bounded stale-key trust window. Requests are rate-limited after identity
resolution and before MCP parsing.

## Operation and stop

Start `cortexd` first with the same durable database used to create the paired
enrollment. Start the gateway in a second process using `CORTEX_GATEWAY_CONFIG`.
Use `Ctrl+C` to stop it: cancellation stops the local listener and outbound
tunnel together. The gateway returns redacted categories such as authentication
failure, invalid configuration, local transport unavailable, or tunnel
unavailable; do not add tokens, enrollment content, or private keys to logs.

If a principal is unpaired, revoked, or lacks a capability grant, the MCP call
is denied and audit evidence is written locally by `cortexd`. Normal paired
write/delete/restore operations need no interactive local approval, but remain
subject to the stored grant, revision, idempotency, and audit checks.
