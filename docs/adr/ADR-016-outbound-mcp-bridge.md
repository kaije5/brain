# ADR-016: Outbound-only bridge for ChatGPT MCP access

## Decision

The local gateway initiates an encrypted outbound tunnel; Cortex exposes no public listener.

## Rationale

Outbound connectivity avoids making the local host directly reachable from the Internet.
