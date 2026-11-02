# ADR-009: Node discovery identity and gateway protocol compatibility

## Status
Accepted — 2026-11-02

## Context

- Issue #73: the CLI should discover MDMA nodes over mDNS instead of needing
  `--node` or `MDMA_NODE`.
- Stage and prod Pis are swapped on one switch port, so an IP can move from one
  node to another. A cached IP is not proof of identity.
- Gateways up to 0.29.0 are already deployed and know nothing about discovery.

## Decision

1. **Discovery.**
   - Nodes announce DNS-SD `_mdma._tcp` on port 5555 through an avahi service
     file, `/etc/avahi/services/mdma.service`.
   - The gateway's runit run script writes it atomically, and only when its
     content changes.
   - TXT records: `id`, `name`, `ver`, `kind=node`, `proto=1`.
   - Why not a custom UDP protocol: avahi already runs on every node, there is
     no new port or firewall rule, macOS supports DNS-SD natively, and the CLI
     can browse with pure Rust (`mdns-sd`).
   - Limitation: discovery is link-local only. It does not cross subnets or
     VPNs. `--node` remains the escape hatch.

2. **Node identity.**
   - The id is the provisioned hostname from `/etc/hostname`, because users
     recognise it.
   - It is NOT the mDNS-advertised name. Avahi renames on conflict:
     `mdma-stage` was observed as `mdma-stage-3`.
   - The run script passes it to the gateway via the env vars `MDMA_NODE_ID` and
     `MDMA_NODE_NAME`, not CLI flags. A mismatched binary and run script then
     cannot crash-loop the gateway.
   - An unconfigured gateway reports no id (`None`), which clients treat as
     unverifiable. It never reports a fake shared value.

3. **Identify request.**
   - `GatewayRequest::Identify` returns
     `GatewayResponse::Identity { identity: NodeIdentity { id, name, version, proto } }`.
   - Clients use it to confirm that a cached IP still belongs to the same node.
   - The CLI never silently switches to a different node.

4. **Gateway wire-compatibility policy.**
   - Gateways up to 0.29.0 answer unknown requests with
     `Error { message: "Invalid request: …" }`. The `INVALID_REQUEST_PREFIX`
     constant is therefore a FROZEN contract. Clients detect "unsupported" by
     that prefix.
   - Sender and detector share one constructor,
     `GatewayResponse::invalid_request`, and a test pins the literal text.
   - Future option, not done: an optional `code: Option<ErrorCode>` field on
     `Error`. It is wire-compatible both ways, but still needs the string
     fallback for old gateways.

5. **Client resolution order** (phase B, for context).
   1. `--node`, then `MDMA_NODE`. These override and do not update the cache.
   2. Local IPC.
   3. The cached node: try the cached IP and Identify; then resolve the name
      fresh, with no stale-cache fallback; then DNS-SD browse matched by id.
   4. Discovery or a pick list.

6. **Later.** The beacon announces `_mdma-beacon._tcp` on port 80. In beacon
   mode the hostname is always `welcome-to-mdma`, so the beacon's id should come
   from the installed system's hostname when it is readable (see #72), and
   otherwise be absent.

## Consequences

### Negative
- Discovery does not work across subnets or VPNs.
- Nodes running gateways older than this change are not discoverable and answer
  Identify with the frozen invalid-request error, so clients must treat that as
  "unverifiable".
- The invalid-request text can never change without breaking old-gateway
  detection.
- The id depends on the provisioned hostname. Renaming a host changes the node's
  identity for cached clients.

## Amendment 2026-11-02: resolution order details (§5)

The original text above is kept as written. Where it conflicts with this
amendment, the amendment wins.

**Trust model:** The identify answer at an IP is the only authority for which
node is at that IP; mDNS/DNS-SD results are candidates, never proof.

- **Unverifiable at the cached IP is accepted only as a last resort.**
  - Unverifiable means an old gateway that doesn't support Identify, or a
    gateway with no id.
  - It is accepted only after the fresh name lookup and the browse by id have
    failed, and only when the cached `.local` name still resolves to that same
    IP. Otherwise the cached node is treated as not found.
  - Rationale: a cache entry is only created from a node that has an id, so an
    unverifiable answer at the cached IP most likely comes from a different node
    holding the same DHCP lease. That is the stage/prod swap.
- **A gateway that supports Identify but returns a service error, or an
  unexpected response, counts as a failed identity check** and is never
  accepted.
- **Browse results are deduplicated by id when one is present,** so an
  avahi-renamed record doesn't list the same node twice.
  - The dedup key is kind plus id, so a beacon that announces the installed
    system's hostname never merges with the node itself.
- **Browse relocation:** a node found by browse with the cached id is accepted
  only after identify at one of its addresses confirms that id. Each announced
  IPv4 except the already-failed cached IP is tried in turn. Unconfirmed
  candidates are ignored.
- **`mdma node select`:**
  - A node whose announced id differs from its identify answer, or that
    announced an id but answers without one, is rejected as an identity mismatch
    and never saved.
  - A node that announced no id may be remembered under the id its gateway
    reports.
- **Worst-case resolve latency:** about 1 s for the cached IP, 1 s for the fresh
  name, 2 s for the browse, plus 1 s per candidate IP.
- **Caveat on "fresh":** fresh name resolution bypasses only MDMA's own disk
  cache. avahi-daemon keeps its own record cache (A-record TTL around 120 s), so
  right after a stage/prod swap the name can briefly still map to the old IP.
  That is acceptable, because the unverifiable fallback also requires that
  mapping and an unverifiable answer.

## Related

- ADR-008 — wifi provisioning; avahi may publish on two interfaces and rename
- Issues #73 (CLI node discovery), #72 (beacon reuses installed wifi)
