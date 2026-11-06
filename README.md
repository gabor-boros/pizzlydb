# pizzlydb

pizzlydb is a transactional, graph-native operational database. It maintains supported graph-query results and delivers durable, resumable subscriptions.

`pizzlydb-storage`, `pizzlydb-core`, and `pizzlydb-graph` are implemented; the other workspace crates are skeletons. Each crate `README.md` describes that crate's contract. `pizzlydb-storage` is the in-house ordered key-value engine (VFS, pager, WAL, and B-tree). `pizzlydb-core` holds identifiers, values, and their ordered key encodings. `pizzlydb-txn` will hold MVCC, validation, and the logical mutation log. `pizzlydb-graph` stores records, catalog names, adjacency, properties, indexes, and constraints as keys in the storage map.

- `pizzlydb-core` — identifiers, typed values, errors, and commit sequence numbers
- `pizzlydb-storage` — page engine and the `StorageEngine` boundary
- `pizzlydb-txn` — MVCC, validation, commit sequencing, and the logical mutation log
- `pizzlydb-graph` — records, adjacency, properties, indexes, and constraints
- `pizzlydb-query` — parser, typed IR, planner, and snapshot executor
- `pizzlydb-incremental` — signed deltas, arrangements, and checkpoints
- `pizzlydb-subscriptions` — snapshot bootstrap, result deltas, and cursors
- `pizzlydb-auth` — identity, permissions, and policy epochs
- `pizzlydb-protocol` — versioned protocol types and canonical error names
- `pizzlydb-server` — runtime composition
- `pizzlydb-sdk` — Rust client
- `pizzlydb-cli` — admin, diagnostics, and query shell
- `pizzlydb-testkit` — reference model and fault harnesses

The `pizzlydb` binary does not open a database yet.

```shell
cargo run -p pizzlydb-cli --quiet
```

## Day-to-day commands

```shell
# Format
cargo fmt --all

# Strict linting
cargo clippy --workspace --all-targets -- -D warnings

# Run tests. Skeleton crates have no tests, so an empty run succeeds.
cargo nextest run --workspace --no-tests=pass

# Run doctests separately
cargo test --workspace --doc

# Generate coverage
cargo llvm-cov --workspace --html

# Audit dependencies
cargo deny check
```

## License

pizzlydb is licensed under the [Apache License 2.0](LICENSE).

## Git hooks

Install [pre-commit](https://pre-commit.com/). `default_install_hook_types` includes `pre-commit` and `commit-msg`, so `pre-commit install` installs both hooks. You can also install them explicitly:

```shell
pre-commit install
pre-commit install --hook-type commit-msg
```

The hooks check Rust formatting, Markdown, YAML, spelling, REUSE, and Conventional Commits. They scan for secrets and misconfiguration with gitleaks, Trivy, and Checkov. GitHub Actions files are checked with actionlint, zizmor, and JSON Schema when those files are present.

## Conventional Commits

Commits follow [Conventional Commits](https://www.conventionalcommits.org/). `feat`, `fix`, and `perf` cut a release. `ci`, `chore`, `docs`, and `test` do not. The commit header is at most 100 characters.

## Continuous integration and releases

On pull requests and pushes to `main`, CI runs formatting, Clippy, nextest, doctests, `cargo deny` for licenses, bans, and sources, `cargo audit`, a CycloneDX SBOM artifact, commitlint, REUSE, and workflow lint.

[Renovate](https://docs.renovatebot.com/) opens weekly grouped pull requests for Cargo, GitHub Actions, and pre-commit updates. Install the [Renovate GitHub App](https://github.com/apps/renovate) on this repository. Non-security updates wait seven days. Security updates are not delayed.

[Release Please](https://github.com/googleapis/release-please) opens a release pull request and syncs `Cargo.lock`. Merging that pull request publishes the GitHub release. The workflow then runs the supply-chain checks, uploads an attested SBOM, and uploads `pizzlydb` archives for Linux (`x86_64` and `aarch64`). Each archive is named `pizzlydb_<version>_<target>.tar.gz`, contains the `pizzlydb` binary and the Apache-2.0 `LICENSE`, and is uploaded with a `.sha256` checksum. Archives are uploaded only after `cargo audit` succeeds.

A fine-grained personal access token in the `RELEASE_PLEASE_TOKEN` secret, with `contents: write` and `pull-requests: write`, lets that release pull request trigger CI. Without that token, GitHub does not start workflows from `GITHUB_TOKEN`.
