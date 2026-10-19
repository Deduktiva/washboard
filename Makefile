# Builds target/packager/Washboard.app (no DMG, unsigned). macOS only, with Xcode 26 for `actool`.
# Same cargo-packager version as the macOS CI job (.github/workflows).
PACKAGER_VERSION := 0.11.8

.PHONY: app fmt check
app: fmt
	cargo build -p washboard-app
	@cargo packager --version 2>/dev/null | grep -q '$(PACKAGER_VERSION)' \
		|| cargo install cargo-packager --version $(PACKAGER_VERSION) --locked
	cargo packager --release -p washboard-app
	@echo "Built target/packager/Washboard.app"

fmt:
	cargo fmt

check: app
	cargo clippy --workspace --all-targets
	RUST_BACKTRACE=1 cargo test --workspace
