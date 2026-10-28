# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build

FUZZ_TIME ?= 30
FUZZ_TOOLCHAIN ?= nightly
# Sanitizers need nightly (-Zsanitizer, -Zbuild-std); pinned like the fuzz job, bump deliberately.
SAN_TOOLCHAIN ?= nightly-2026-10-15
SAN_TARGET ?= $(shell rustc -vV | sed -n 's/^host: //p')

.PHONY: lint test test-rust test-mac mac verify-bundle dmg project fuzz sanitize sanitize-address sanitize-thread miri prepush hooks

lint:
	scripts/check-repo.sh
	cd engine && cargo fmt --all --check
	cd engine && cargo clippy --workspace --all-targets -- -D warnings
	cd engine && cargo deny check
	cd engine/fuzz && cargo fmt --check && cargo clippy --locked -- -D warnings
	swift format lint --strict -r mac/

test: test-rust test-mac

# Installs the pre-push hook (scripts/hooks/pre-push): a push is refused unless scripts/prepush.sh
# passed for the commit being pushed. core.hooksPath is shared by every worktree of this clone and
# replaces .git/hooks; bypass once with `--no-verify`.
hooks:
	git config core.hooksPath scripts/hooks
	@echo "pre-push hook installed"

# Pre-push gate; picks checks from the diff against origin/main (see scripts/prepush.sh).
prepush:
	scripts/prepush.sh

test-rust:
	cd engine && cargo test --workspace

# Short fuzz run per target (nightly + cargo-fuzz). Fixtures and fuzz/seeds seed the corpus;
# crashes land in engine/fuzz/artifacts/.
fuzz:
	mkdir -p engine/fuzz/corpus/show_file engine/fuzz/corpus/decode engine/fuzz/corpus/osc \
		engine/fuzz/corpus/osc_session engine/fuzz/corpus/protocol
	cd engine && cargo +$(FUZZ_TOOLCHAIN) fuzz run show_file fuzz/corpus/show_file fixtures/show fuzz/seeds \
		-- -max_total_time=$(FUZZ_TIME) -dict=fuzz/show_file.dict
	cd engine && cargo +$(FUZZ_TOOLCHAIN) fuzz run decode fuzz/corpus/decode crates/audio/tests/fixtures fuzz/decode-seeds \
		-- -max_total_time=$(FUZZ_TIME) -max_len=65536
	cd engine && cargo +$(FUZZ_TOOLCHAIN) fuzz run osc fuzz/corpus/osc fuzz/osc-seeds \
		-- -max_total_time=$(FUZZ_TIME) -max_len=65536
	cd engine && cargo +$(FUZZ_TOOLCHAIN) fuzz run osc_session fuzz/corpus/osc_session fuzz/osc-session-seeds \
		-- -max_total_time=$(FUZZ_TIME) -max_len=65536 -timeout=10
	cd engine && cargo +$(FUZZ_TOOLCHAIN) fuzz run protocol fuzz/corpus/protocol fixtures/protocol/client \
		fixtures/protocol/server fuzz/protocol-seeds -- -max_total_time=$(FUZZ_TIME) -max_len=65536 -timeout=10

# Nightly CI and local: engine tests under ASan and TSan, then Miri on openlab-core (QA-09, docs/ARCHITECTURE.md §11).
# Needs: rustup toolchain install $(SAN_TOOLCHAIN) --profile minimal -c rust-src -c miri
# Rust has no -Zsanitizer=undefined; Miri is the undefined-behaviour check, and runs on core only (no I/O, no FFI).
# --lib --tests skips doctests (rustdoc does not support -Zsanitizer reliably). Ignored tests (real-time, perf,
# soak, CPU budget) stay ignored: their bounds are wall-clock and mean nothing under a 2-10x slowdown.
# Skipped by name, each because a sanitizer cannot give a meaningful answer:
#   ThreadSanitizer: long_start_chains_validate_in_linear_time asserts a wall-clock bound (a ~5-15x TSan slowdown
#   breaks it; the algorithm is not at issue). It passes under ASan, so only the thread leg skips it.
SAN_TEST := cargo +$(SAN_TOOLCHAIN) test -Zbuild-std --target $(SAN_TARGET) --workspace --locked --lib --tests --no-fail-fast
sanitize: sanitize-address sanitize-thread miri

sanitize-address:
	cd engine && RUSTFLAGS="-Zsanitizer=address" RUSTDOCFLAGS="-Zsanitizer=address" $(SAN_TEST)

# TSan needs every crate instrumented, hence -Zbuild-std; on macOS it is not supported for all targets.
sanitize-thread:
	cd engine && RUSTFLAGS="-Zsanitizer=thread" RUSTDOCFLAGS="-Zsanitizer=thread" $(SAN_TEST) -- --skip long_start_chains_validate_in_linear_time

# Subset: core unit tests with a short proptest budget (Miri is ~100x slower). Skipped by name:
#   too_many_workspaces_are_truncated_quickly: serialises thousands of settings entries; hours under Miri.
MIRI_SKIP := --skip too_many_workspaces_are_truncated_quickly
miri:
	cd engine && PROPTEST_CASES=4 MIRIFLAGS="-Zmiri-disable-isolation" cargo +$(SAN_TOOLCHAIN) miri test -p openlab-core --lib --locked -- $(MIRI_SKIP)

test-mac: project
	$(XCODEBUILD) -destination "platform=macOS,arch=$$(uname -m)" test

mac: project
	$(XCODEBUILD) -configuration Release build
	@echo "Built build/Build/Products/Release/openLab.app"

verify-bundle:
	mac/scripts/verify-bundle.sh build/Build/Products/Release/openLab.app

dmg:
	@echo "dmg: not yet implemented (M6.1)"; exit 1

project:
	cd mac && xcodegen --quiet
