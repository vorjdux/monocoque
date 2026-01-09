# Publishing to Crates.io

This guide documents the process for publishing Monocoque crates to crates.io.

## Pre-Publication Checklist

### 1. Documentation

-   [x] All public APIs have rustdoc comments
-   [x] Examples compile and run correctly
-   [x] README.md is comprehensive
-   [x] CHANGELOG.md exists and is up-to-date
-   [x] Blueprint documentation is complete

### 2. Cargo.toml Metadata

All crates must have:

-   [x] `description`
-   [x] `keywords` (max 5)
-   [x] `categories`
-   [x] `license`
-   [x] `repository`
-   [x] `homepage`
-   [x] `documentation`
-   [x] `readme`
-   [x] `rust-version`

### 3. Code Quality

-   [x] All tests pass: `cargo test --workspace --all-features`
-   [x] Clippy passes: `cargo clippy --workspace --all-features`
-   [x] Documentation builds: `cargo doc --workspace --no-deps`
-   [x] No uncommitted changes (or use `--allow-dirty` for testing)

### 4. Error Handling

-   [x] All error types use `thiserror`
-   [x] Result type aliases defined (`pub type Result<T>`)
-   [x] Errors documented in API docs (`# Errors`)

### 5. Version Control

-   [x] `Cargo.lock` in `.gitignore` (proper for libraries)
-   [x] All changes committed to git
-   [x] Version tags created (`v0.1.0`)

## Publication Order

Crates must be published in dependency order:

```
1. monocoque-core    (no internal dependencies)
2. monocoque-zmtp    (depends on monocoque-core)
3. monocoque         (depends on monocoque-core, monocoque-zmtp)
```

## Publishing Commands

### Dry Run (Test Without Publishing)

```bash
# Test each crate in order
cargo publish --dry-run -p monocoque-core
cargo publish --dry-run -p monocoque-zmtp
cargo publish --dry-run -p monocoque
```

### Actual Publication

```bash
# Login to crates.io (one-time)
cargo login

# Publish in dependency order
cargo publish -p monocoque-core
cargo publish -p monocoque-zmtp
cargo publish -p monocoque
```

**Important**: Wait ~1 minute between publishes to allow crates.io to index the dependencies.

## Post-Publication

1. **Tag the release**:

    ```bash
    git tag -a v0.1.0 -m "Release v0.1.0"
    git push origin v0.1.0
    ```

2. **Create GitHub release**:

    - Go to https://github.com/vorjdux/monocoque/releases
    - Create release from tag
    - Copy CHANGELOG.md content

3. **Update documentation**:

    - Docs.rs will automatically build documentation
    - Verify at https://docs.rs/monocoque

4. **Announce**:
    - Reddit: r/rust
    - Twitter/X
    - This Week in Rust newsletter

## Version Updates

Follow [Semantic Versioning](https://semver.org/):

-   **Patch** (0.1.1): Bug fixes, no API changes
-   **Minor** (0.2.0): New features, backward compatible
-   **Major** (1.0.0): Breaking changes

Update versions in `Cargo.toml` workspace section:

```toml
[workspace.package]
version = "0.2.0"
```

## Yanking Releases

If a critical bug is found:

```bash
# Yank the broken version
cargo yank --version 0.1.0 monocoque

# Publish fixed version
cargo publish -p monocoque
```

## Common Issues

### "crate X depends on Y that does not exist"

**Solution**: Publish dependencies first, wait for indexing.

### "no verified email address"

**Solution**: Verify your email on crates.io.

### "authentication token not found"

**Solution**: Run `cargo login` with your crates.io API token.

### "uncommitted changes"

**Options**:

-   Commit your changes (recommended)
-   Use `--allow-dirty` flag for testing only

## Maintenance

### Security Advisories

-   Monitor https://rustsec.org/
-   Update dependencies regularly: `cargo update`
-   Run `cargo audit` before releases

### Dependency Updates

```bash
# Check for outdated dependencies
cargo outdated

# Update within semver constraints
cargo update

# Update to latest (may break)
cargo upgrade
```

## Resources

-   [Cargo Book - Publishing](https://doc.rust-lang.org/cargo/reference/publishing.html)
-   [Crates.io Policies](https://crates.io/policies)
-   [API Guidelines](https://rust-lang.github.io/api-guidelines/)
-   [Rust RFC 1105 - API Evolution](https://rust-lang.github.io/rfcs/1105-api-evolution.html)

## Support

For publication issues:

-   Crates.io help: help@crates.io
-   Cargo GitHub: https://github.com/rust-lang/cargo/issues
