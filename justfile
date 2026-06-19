# Run all CI checks (format, clippy, deny, all tests)
ci: fmt-check clippy check-facades deny test

# Run quick CI checks (format, clippy, fast tests only)
ci-quick: fmt-check clippy check-facades test-quick

# Format check
fmt-check:
	cargo fmt --all -- --check

# Auto-format code
fmt:
	cargo fmt --all

# Run clippy lints (fail on warnings)
clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

# Run all tests
test:
	cargo test --workspace --locked

# Run tests excluding ones tagged `__quick_excluded` in the function name
test-quick:
	cargo test --workspace --locked -- --skip __quick_excluded

# Build release binary
build:
	cargo build --release

# Clean build artifacts
clean:
	cargo clean

# Enforce mod.rs facade integrity
check-facades:
	./scripts/check-facades.sh

# Run cargo-deny bans and advisories checks
deny:
	cargo deny check bans advisories
