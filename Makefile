# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build
SPDX := SPDX-License-Identifier: GPL-3.0-or-later

FUZZ_TIME ?= 30
FUZZ_TOOLCHAIN ?= nightly

.PHONY: lint test test-rust test-mac mac verify-bundle dmg project fuzz prepush

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
