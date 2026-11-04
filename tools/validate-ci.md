# Local CI validation

Run before staging any change that touches `.github/workflows/`:

```
powershell -NoProfile -File tools\validate-ci.ps1
```

Requires Python 3 and Go on PATH (first run only; tools go to `%LOCALAPPDATA%\kon-ci-tools`, nothing global, nothing in the repo).

| Layer | Tool | Covers |
|---|---|---|
| YAML syntax/style | yamllint (`relaxed` preset; `truthy` key check off because `on:` is a valid Actions key) | parse errors, indentation, duplicate keys, trailing spaces |
| Workflow semantics | actionlint | schema, `${{ }}` expressions, runner labels, action inputs, matrix/needs, contexts |
| Job simulation | `act` (`-Simulate`) | dry-run of the job graph; **needs Docker** |

## Cannot be reproduced locally

- **GitHub-hosted runners.** `act` (Docker, Linux containers) is never equivalent to them. It cannot run the `windows-latest`, `macos-26-intel` or `macos-latest` matrix legs at all, so those OS-specific results (and the cross-platform ELLIE determinism test) only exist on GitHub.
- **Docker.** Not installed on this machine, so `act` is reported SKIPPED, not passed.
- **Runner-label availability.** actionlint only knows labels at its release date; a brand-new label can be flagged, or a retired one missed. Confirm against GitHub's runner docs.
- **External state.** Checking out the private/public `Kernel-of-Optimized-Networks-KON/pkon` repo, the `dng-corpus` cache (~1.1 GB, raw.pixls.us), secrets, permissions and branch protection.
- **Pinned versions** (`numpy`, `tifffile`, `imagecodecs`, action tags) are not checked for existence on PyPI/GitHub.
- actionlint's `shellcheck` and `pyflakes` integrations are inactive unless those tools are installed.
