# NightBloom end-to-end tests

From the repository root, with Python/uv and Node.js/npm installed:

The browser suite requires the owner's frozen science fixtures, which are **not
distributed** with the source or Windows packages. Unit tests, native synthetic
contracts and production builds do not need them. Put the reference sessions under
`data\test-fixtures\sessions`, or set `ASTROIMP_E2E_SOURCE_DATA` to a separate data
folder containing that layout. Never point tests at editable live sessions.

```powershell
Set-Location frontend
npm ci
npx playwright install chromium
npm run build
npm run e2e
```

The single React project runs serially in headless Chromium/SwiftShader.
About 55 tests (2026-10); performance and detail-timing specs are opt-in via env vars
and otherwise skipped. Do not add tests that assert IDs, classes or layout; see
`AGENTS.md` → Testing policy.

Setup copies the frozen reference sessions from `data\test-fixtures\sessions` into
`frontend\e2e\.run\<run-id>`, decimates the copied projects, and starts its own
NightBloom server on a free port. It never changes the originals or the server on 8765.
The Home workflow uses the JWST session through its recent-session card, so it
is not unused and remains in setup. Tests explicitly PUT
deterministic edits before opening the editor; they never assume the owner's
current saved sliders, transforms, or masks.
Live capture, Aladin, builds/downloads, and snapshot refresh are blocked; previews
and build plans are fixtures. The shared download-cache junction is read-only
during tests. Teardown stops only the fixture server and deletes copied data.

Failure screenshots and traces are in `frontend\e2e\.results\<run-id>`.
There are no screenshot-generation or theme-sweep tests. Coverage retains
GPU/NumPy blend and edge/brush parity, export geometry, real contact
sheets, live-state probes, canvas interactions, autosave recovery and Sky triage.
Store tests own undo/redo, bulk edits, saved-edit reconciliation and migration;
backend tests own build math, native export accuracy/cancellation and data safety.

## All suites

From the repository root:

```powershell
uv run --no-sync --with pytest python -m pytest -q
Set-Location frontend
npm test
npm run build
npm run e2e
```

Historical record of the 2026-10-24 pruning:

| Suite (executions) | Before | After | Removed/merged | Simplified | Kept unchanged |
| --- | ---: | ---: | ---: | ---: | ---: |
| Python | 113 | 101 | 12 | 8 | 93 |
| Vitest | 47 | 45 | 2 | 2 | 43 |
| Playwright | 51 | 26 | 25 | 17 | 9 |

Counts come from the actual suites, not the review's summary. Merged browser
tests count as one retained, simplified execution and one removed execution.
Rejected duplicate claims: `needs_build` does not cover a fresh startup without
npm; generic star history does not cover resetting star controls/overrides;
bulk visibility does not cover Hide others; preference serialization does not
cover live System-theme updates. Those distinct contracts remain. The review's
unused-session claim was also rejected: Home's recent card opens that session.

Measured end-to-end wall time, including setup and teardown: **332.1 seconds
before → 140.6 seconds after**. The original suite finished with 49 passes and
2 failures (outdated Build button expectations and a fixture route-teardown
race); the pruned suite passed all 26 tests. The baseline overlapped a diagnostic
run, so these are observed wall times, not a controlled performance benchmark.
