# Publishing to Crates.io

## Pre-publish checklist

Before publishing any crate, verify:

- All public APIs have rustdoc comments, including `# Errors` sections where relevant
- Examples compile and run correctly
- CHANGELOG.md is up to date
- `cargo test --workspace --all-features` passes
- `cargo clippy --workspace --all-features` passes
- `cargo doc --workspace --no-deps` builds cleanly
- `cargo audit` shows no unresolved advisories
- All changes are committed to git

## Publication order

Crates have internal dependencies and must be published in this order:

1. `monocoque-core` — no internal dependencies
2. `monocoque-zmtp` — depends on monocoque-core
3. `monocoque` — depends on monocoque-core and monocoque-zmtp

Wait roughly a minute between publishes so crates.io finishes indexing each one before the next is submitted.

## Commands

```bash
# Dry run — validates packaging without uploading
cargo publish --dry-run -p monocoque-core
cargo publish --dry-run -p monocoque-zmtp
cargo publish --dry-run -p monocoque

# Authenticate (one-time setup)
cargo login

# Publish in order
cargo publish -p monocoque-core
cargo publish -p monocoque-zmtp
cargo publish -p monocoque
```

## After publishing

Tag the release and push:

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

Create a GitHub release from the tag and paste the relevant CHANGELOG section as the description. Docs.rs will pick up the new version automatically; check https://docs.rs/monocoque once indexing finishes.

## Version bumps

Follow semver. Update the shared version in the workspace `Cargo.toml`:

```toml
[workspace.package]
version = "0.2.0"
```

Patch (0.1.x) for bug fixes, minor (0.x.0) for backward-compatible additions, major (x.0.0) for breaking changes.

## Yanking a broken release

```bash
cargo yank --version 0.1.0 monocoque
# Then fix the bug and publish a patch release
cargo publish -p monocoque
```

## Common errors

**"crate X depends on Y that does not exist"** — you published out of order, or crates.io hasn't finished indexing yet. Wait a minute and retry.

**"no verified email address"** — verify your email on crates.io before publishing.

**"authentication token not found"** — run `cargo login` with your API token from crates.io account settings.

**"uncommitted changes"** — commit first. Pass `--allow-dirty` only for local dry-run testing.
