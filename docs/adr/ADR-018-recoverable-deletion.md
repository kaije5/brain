# ADR-018: Recoverable deletion lifecycle

## Decision

Deletion transitions entities through a recoverable lifecycle before purge.

## Rationale

Users can reverse accidental destructive actions without hiding audit history.
