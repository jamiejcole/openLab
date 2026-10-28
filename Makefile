# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build
SPDX := SPDX-License-Identifier: GPL-3.0-or-later

FUZZ_TIME ?= 30
FUZZ_TOOLCHAIN ?= nightly
# Sanitizers need nightly (-Zsanitizer, -Zbuild-std); pinned like the fuzz job, bump deliberately.
SAN_TOOLCHAIN ?= nightly-2026-10-15
SAN_TARGET ?= $(shell rustc -vV | sed -n 's/^host: //p')

.PHONY: lint test test-rust test-mac mac verify-bundle dmg project fuzz sanitize sanitize-address sanitize-thread miri prepush

lint:
	@missing=$$(git ls-files -co --exclude-standard '*.rs' '*.swift' '*.sh' '*.yml' Makefile | xargs grep -L "$(SPDX)"); \
		if [ -n "$$missing" ]; then echo "Missing SPDX header:"; echo "$$missing"; exit 1; fi
	cd engine && cargo fmt --all --check
	cd engine && cargo clippy --workspace --all-targets -- -D warnings
	cd engine && cargo deny check
	cd engine/fuzz && cargo fmt --check && cargo clippy --locked -- -D warnings
	swift format lint --strict -r mac/

test: test-rust test-mac

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
#   (none yet; add `--skip name` here with the reason)
SAN_TEST := cargo +$(SAN_TOOLCHAIN) test -Zbuild-std --target $(SAN_TARGET) --workspace --locked --lib --tests --no-fail-fast
sanitize: sanitize-address sanitize-thread miri

sanitize-address:
	cd engine && RUSTFLAGS="-Zsanitizer=address" RUSTDOCFLAGS="-Zsanitizer=address" $(SAN_TEST)

# TSan needs every crate instrumented, hence -Zbuild-std; on macOS it is not supported for all targets.
sanitize-thread:
	cd engine && RUSTFLAGS="-Zsanitizer=thread" RUSTDOCFLAGS="-Zsanitizer=thread" $(SAN_TEST)

# Subset: core unit tests, a short proptest budget (Miri is ~100x slower), no file-system isolation break needed.
miri:
	cd engine && PROPTEST_CASES=4 MIRIFLAGS="-Zmiri-disable-isolation" cargo +$(SAN_TOOLCHAIN) miri test -p openlab-core --lib --locked

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
