# MIGRATION

## xdelta -> Oxidelta Transition

Oxidelta is format-compatible with xdelta3 for supported modes, but the CLI is intentionally subcommand-first and Rust-idiomatic.

## Command Translation

| Legacy xdelta | Oxidelta |
|---|---|
| `xdelta -e -s old new patch` | `oxidelta encode --source old new patch` |
| `xdelta -d -s old patch out` | `oxidelta decode --source old patch out` |
| `xdelta config` | `oxidelta config` |
| `xdelta printhdr patch.vcdiff` | `oxidelta header patch.vcdiff` |
| `xdelta printhdrs patch.vcdiff` | `oxidelta headers patch.vcdiff` |
| `xdelta printdelta patch.vcdiff` | `oxidelta delta patch.vcdiff` |
| `xdelta merge -m a -m b c out` | `oxidelta merge --patch a --patch b c out` |

## Flag Translation

| Legacy flag | Oxidelta flag |
|---|---|
| `-f` | `--force` |
| `-c` | `--stdout` |
| `-s <file>` | `--source <file>` |
| `-n` | `--no-checksum` |
| `-J` | `--check-only` |
| `-S <codec>` | `--secondary <codec>` |
| `-W <size>` | `--window-size <size>` |
| `-B <size>` | `--source-window-size <size>` |
| `-P <size>` | `--duplicate-window-size <size>` |
| `-I <size>` | `--instruction-buffer-size <size>` |
| `-0..-9` | `--level 0..9` |

## Workflow Conversion Script

Use:

```bash
scripts/migrate-from-xdelta.sh path/to/workflows-or-scripts
```

The script performs mechanical replacements and writes `*.migrated` files for review.

## Performance Comparison Guide

When comparing old workflows (`xdelta`) to new ones (`oxidelta`):

1. Reuse identical source/target datasets.
2. Keep compression settings equivalent (`level`, checksum, secondary mode).
3. Compare:
   - wall-clock encode/decode times
   - resulting delta size
   - end-to-end pipeline time in your CI/CD context
4. Use multiple runs and compare medians.

## Migration Strategy

1. Convert scripts using `scripts/migrate-from-xdelta.sh`.
2. Run interoperability checks on sample datasets.
3. Switch CI to `oxidelta` commands.
4. Keep xdelta fallback in early rollout if your process is high risk.
5. Remove fallback once patch validation is stable.
