# feat(claude-science): add Claude Science adapter

Adds a new `claude-science` usage source that reads per-frame token usage
from the Claude Science desktop app's local SQLite metadata database.

## What it does

- `ccusage claude-science <daily|monthly|session>` — per-agent reports
- Included in the unified report and `--by-agent` breakdowns
- `claudeScience` config section with per-command defaults (same shape as
  other basic agents)

## Implementation notes

- **Record shape**: Claude Science stores one row per conversation frame
  (a root conversation or a delegated sub-agent turn) with aggregate token
  counts, not per-message records. The adapter emits one usage entry per
  frame. `root_frame_id` (falling back to `id`) maps to the session id, so
  sub-agent frames roll up into their parent session in session reports.
- **Discovery**: `CLAUDE_SCIENCE_DB=/path/to/metadata.db` (comma-separated
  paths allowed) takes precedence; otherwise well-known roots under the
  user's home directory are scanned, bounded to two levels, skipping
  `conda`/`pkgs`/`node_modules` and hidden directories. A database
  qualifies when it exposes a `frames` table with an `input_tokens`
  column; everything else is skipped.
- **Costs**: model names may carry a routing prefix (e.g.
  `cs-switch-direct:<model>`); the prefix is stripped before pricing
  lookups. Under `--cost auto` the platform's own recorded `total_cost` is
  preferred whenever non-NULL; the pricing catalog is consulted only for
  frames with a NULL recorded cost. `--cost display` always reports the
  recorded cost.
- **Safety**: the database is opened read-only; all access is `SELECT`-only.
  Databases that fail the schema probe are ignored rather than erroring.

## Known limitation

Claude Science's database is an internal format that may change without
notice. The adapter is designed to fail soft (skip non-matching
databases), but report accuracy across app versions is not guaranteed.
If maintainers prefer, this could ship behind the existing
`CLAUDE_SCIENCE_DB` opt-in only (i.e. no auto-discovery).

## Testing

- Unit tests for discovery (schema probe, env override) and model
  prefix normalization
- Fixture-based integration test asserting daily/monthly/session
  aggregation against a synthetic database
- End-to-end CLI snapshot tests (`claude_science_cli`), mirroring the
  existing `zcode_cli` tests
- `cargo fmt --check`, `cargo clippy` and the full `cargo test` suite pass

Closes #(issue number)
