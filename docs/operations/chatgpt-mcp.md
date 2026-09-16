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

## Pair a remote OIDC subject as the local owner

Creating a remote principal is a trusted local-owner operation. Start
`cortexd`, set `CORTEX_DATABASE` in a second shell to the same private database,
then run the authenticated local remote-enrollment command below. It creates a distinct durable
principal, grants only the named capabilities, writes a protected IPC enrollment
file, and records a redacted local audit event. It prints the principal ID,
subject, requested grants, enrollment **path**, correlation ID, and
`restart_required`; it never prints an enrollment signing key.

```powershell
$env:CORTEX_DATABASE = 'C:\CortexData\cortex.db'
cargo run -p brain -- --output json remote enroll --subject 'chatgpt-owner-subject' --grant cortex_knowledge_create --grant cortex_knowledge_search --grant cortex_memory_search
```

The exact same subject/grant set is idempotent: retrying it returns the existing
principal and enrollment path. A changed grant set for that subject is rejected;
make an explicit policy/provisioning change instead. The command is accepted
only through the authenticated local owner IPC enrollment. A remote or unpaired
client cannot create principals, and a newly enrolled remote identity cannot
use grants it was not explicitly given.

Before the protected enrollment artifact or verifier manifest is written, the
daemon commits one local SQLite transaction containing the subject-to-principal
mapping, requested grants, operation replay record, and redacted success audit
event. It then reconciles the protected artifact from that durable mapping. If
the local file write is interrupted, the command returns a redacted unavailable
result; re-run the same owner command to reconcile the same identity. It never
creates a second principal or prints the private enrollment key.
At startup, the daemon accepts a local verifier-manifest entry only when it
exactly matches that committed mapping; a copied or forged manifest cannot
bootstrap a remote principal or grant.

Stop `cortexd` with `Ctrl+C`, then start it again using the same
`CORTEX_DATABASE` before starting the gateway. The restart loads the new
protected verifier and grants; it is required before the enrollment can
authenticate. Keep the returned enrollment file local and private.

Before enabling the gateway, an operator must also provide all of these:

1. A relay that implements the v0.1 registration/forwarding contract and
   presents a certificate trusted by the gateway.
2. A client certificate/key pair for mTLS, stored in protected local files.
3. An OIDC issuer, audience, permitted signing algorithms, and a stable subject
   for the remote identity.
4. The principal ID and protected IPC enrollment path returned by the local
   enrollment command, mapped to the same stable OIDC subject.
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

Replace the `paired_subjects` object with the returned
`gateway_paired_subject` object from the local-owner enrollment command. It is
safe to place the path in this configuration; do not place the enrollment file
contents, token, or private key there.

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
