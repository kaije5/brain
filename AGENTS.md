# Agent workflow

## Branching model

short-lived **feature branches** per story

```text
main
  ├── feat/<story-key>-<slug>    one branch per story, e.g. feat/scrum-41-model-registry
  ├── feat/scrum-42-router
  └── ...
```


- **Feature branch**: named `feat/<story-key>-<slug>` using the Jira story key
  (e.g. `feat/scrum-40-model-routing-boundary`). Branched **from the version
  branch**, not from `main`. One branch per story; keep it focused and short-lived.
- **Merging up**: a story merges via PR (lint + tests
  must pass).
- **Syncing**: if the branch moves while a story is in flight, rebase or
  merge the branch into the branch to stay current.
- **Fixes and CI chores** follow the same rule:
  `fix/<story-key>-<slug>` or `chore/<slug>` branched from and merged

## Commits

Use Conventional Commit prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `ci:`)
and reference the story key in the body when applicable.

## Jira status tracking

Jira is the source of truth for story status. Always keep it in sync with
reality:

- Before starting work on a story, transition it to the in-progress status.
- As soon as a story is implemented and verified (PR open with CI green, or
  merged), transition it to Done — never leave implemented stories silently
  sitting in the backlog.
- Record deviations from a story's acceptance criteria on the issue (comment
  or linked follow-up story), not only in the PR.
- Reference the story key in commit messages and PR descriptions.

## Verification before merging

Before opening a PR (at any level), run:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo nextest run --workspace
```
