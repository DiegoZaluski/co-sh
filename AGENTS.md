# Agent instructions

## Building and testing on a memory-constrained machine

This repo's `.cargo/config.toml` caps build parallelism and test parallelism
(`jobs = 2`, `RUST_TEST_THREADS = 4`) because the dev machine has 13 GB of RAM
and freezes under a full `cargo test --workspace` at default parallelism. The
caps apply automatically to any `cargo` command run from inside the repo —
**use plain `cargo test` as you normally would; do not add `--test-threads`
or `-j` flags yourself**, they are already handled.

- Prefer scoped runs: `cargo test -p cosh-tools --lib fs::` instead of the
  whole workspace when you only changed one module.
- Do NOT delete or override `.cargo/config.toml` and do NOT run cargo with
  `CARGO_BUILD_JOBS` / `RUST_TEST_THREADS` env overrides — a full-parallelism
  run OOM-kills the machine (verified: `rustc` gets SIGKILLed by the kernel).
- If the machine feels sluggish mid-build, it is swap pressure; wait instead
  of starting a second cargo process in parallel.

# COMMIT INSTRUCTION.

Follow the project's commit conventional: `type(escopo)`

## Rules
- Use only valid scopes. Never invent or use non-existent scopes.
- Write the commit body in prose.
- Commits involving multiple tasks must be organized into separate paragraphs.
- Commits involving multiple scopes must omit the scope from the header, keeping only the type.
- Keep commits well-organied and properly formattted, avoiding excessively long lines.
