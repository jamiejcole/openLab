# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build
# A hung test fails by name in minutes, not as a job stuck until the runner's limit; a hang the spindump
# proves is the runner's WindowServer, not the test, runs again once (scripts/run-mac-tests.sh).
MAC_TESTS_WITH_LIMIT := scripts/run-mac-tests.sh $(XCODEBUILD) -destination "platform=macOS,arch=$$(uname -m)" \
	-test-timeouts-enabled YES -default-test-execution-time-allowance 60
# The per-PR UI tests (scripts/ui-smoke.txt) as -only-testing arguments.
UI_SMOKE := $(foreach t,$(shell grep -Ev '^[[:space:]]*(\#|$$)' scripts/ui-smoke.txt),-only-testing:$(t))
# The unit test classes `make test-mac` runs in its second test process.
LATE_TEST_CLASSES := $(shell grep -hoE 'class [M-Z][A-Za-z0-9_]*: XCTestCase' mac/openLabTests/*.swift | sed 's/class //; s/:.*//' | sort -u)
# The everyday-flow tests (scripts/core-flows.txt), unit and UI, as -only-testing arguments.
CORE_FLOWS := $(foreach t,$(shell grep -Ev '^[[:space:]]*(\#|$$)' scripts/core-flows.txt),-only-testing:$(t))

FUZZ_TIME ?= 30
FUZZ_TOOLCHAIN ?= nightly
# Sanitizers need nightly (-Zsanitizer, -Zbuild-std); pinned like the fuzz job, bump deliberately.
SAN_TOOLCHAIN ?= nightly-2026-10-15
SAN_TARGET ?= $(shell rustc -vV | sed -n 's/^host: //p')

.PHONY: ui-shots lint test test-rust test-mac test-mac-unit test-mac-core-flows test-mac-ui-full mac verify-bundle test-bundle-rebuild dmg project fuzz sanitize sanitize-address sanitize-thread miri prepush hooks

lint:
	scripts/check-repo.sh
	# Same command lines as CI (scripts/rust-checks.sh): the test build, then the shipped one (--lib --bins).
	scripts/rust-checks.sh fmt
	scripts/rust-checks.sh clippy
	cd engine && cargo deny check
	scripts/rust-checks.sh fuzz-lint
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
	scripts/rust-checks.sh test

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

# Nightly CI and local: engine tests under ASan and TSan, then Miri on openlab-core (QA-09).
# Needs: rustup toolchain install $(SAN_TOOLCHAIN) --profile minimal -c rust-src -c miri
# Rust has no -Zsanitizer=undefined; Miri is the undefined-behaviour check, and runs on core only (no I/O, no FFI).
# --lib --tests skips doctests (rustdoc does not support -Zsanitizer reliably). Ignored tests (real-time, perf,
# soak, CPU budget) stay ignored: their bounds are wall-clock and mean nothing under a 2-10x slowdown.
# Skipped by name, each because a sanitizer cannot give a meaningful answer:
#   ThreadSanitizer: long_start_chains_validate_in_linear_time builds a 200k-cue chain, and TSan's ~5-15x
#   slowdown made it miss its old time bound. That bound is gone, so the skip may be unneeded; it stays until
#   the sanitizer job has run without it. It passes under ASan, so only the thread leg skips it.
SAN_TEST := cargo +$(SAN_TOOLCHAIN) test -Zbuild-std --target $(SAN_TARGET) --workspace --locked --lib --tests --no-fail-fast
sanitize: sanitize-address sanitize-thread miri

sanitize-address:
	cd engine && RUSTFLAGS="-Zsanitizer=address" RUSTDOCFLAGS="-Zsanitizer=address" $(SAN_TEST)

# TSan needs every crate instrumented, hence -Zbuild-std; on macOS it is not supported for all targets.
sanitize-thread:
	cd engine && TSAN_OPTIONS="suppressions=$(CURDIR)/engine/tsan.supp" RUSTFLAGS="-Zsanitizer=thread" RUSTDOCFLAGS="-Zsanitizer=thread" $(SAN_TEST) -- --skip long_start_chains_validate_in_linear_time

# Subset: core unit tests with a short proptest budget (Miri is ~100x slower). Skipped by name:
#   too_many_workspaces_are_truncated_quickly: serialises thousands of settings entries; hours under Miri.
MIRI_SKIP := --skip too_many_workspaces_are_truncated_quickly
miri:
	cd engine && PROPTEST_CASES=4 MIRIFLAGS="-Zmiri-disable-isolation" cargo +$(SAN_TOOLCHAIN) miri test -p openlab-core --lib --locked -- $(MIRI_SKIP)

# The engine-free unit tests: what every app PR runs. The tests that start the real engine report a
# skip (OPENLAB_ENGINE_TESTS=skip reaches the test process through xcodebuild's TEST_RUNNER_ prefix).
test-mac-unit: project
	TEST_RUNNER_OPENLAB_ENGINE_TESTS=skip $(MAC_TESTS_WITH_LIMIT) -- -only-testing:openLabTests

# Every unit test, real-engine ones included, plus the UI smoke set (scripts/ui-smoke.txt): what a PR
# that changes the engine, the protocol, the app's engine client, its tests or project runs (scripts/paths.sh).
# Two test processes: later classes ran about twice as fast in a fresh one when a hang forced a restart
# (one process gathers windows and connections over 1,100 tests). Both always run, so a failure in the
# first still reports the second's, except after a run cut short by a proven stall (exit 75 of
# scripts/run-mac-tests.sh): the runner is wedged, so the second would most likely hang to the step's limit. A class the name match misses stays in the first run, so it still runs.
test-mac: project
	@test -n "$(UI_SMOKE)" || { echo "scripts/ui-smoke.txt lists no tests"; exit 1; }
	@test -n "$(LATE_TEST_CLASSES)" || { echo "no test classes for the second test process"; exit 1; }
	rc=0; \
	$(MAC_TESTS_WITH_LIMIT) -- -only-testing:openLabTests $(foreach c,$(LATE_TEST_CLASSES),-skip-testing:openLabTests/$(c)) || rc=$$?; \
	if [ "$$rc" = 75 ]; then echo "test-mac: the first test process was cut short by a WindowServer stall on the runner; not starting the second. Re-run the job."; exit $$rc; fi; \
	if [ "$$rc" = 76 ]; then echo "test-mac: a test hung in the first test process; not starting the second, which would most likely hang to the step's limit. Read the hang report in the result bundle, then re-run the job."; exit $$rc; fi; \
	$(MAC_TESTS_WITH_LIMIT) -- $(foreach c,$(LATE_TEST_CLASSES),-only-testing:openLabTests/$(c)) $(UI_SMOKE) || rc=$$?; \
	exit $$rc

# The everyday QLab-user flows on the real engine (scripts/core-flows.txt): nightly before the full suite,
# and on demand after a big feature change.
test-mac-core-flows: project
	@test -n "$(CORE_FLOWS)" || { echo "scripts/core-flows.txt lists no tests"; exit 1; }
	$(MAC_TESTS_WITH_LIMIT) -- $(CORE_FLOWS)

# Every unit test and the whole UI suite: what the nightly run does.
test-mac-ui-full: project
	$(MAC_TESTS_WITH_LIMIT) --

# Pictures of the real workspace window driven through a scenario on the real engine, drawn offscreen
# (no focus taken): `make ui-shots SCENARIO=mac/scenarios/<name>.json [OUT=<dir>]`. It waits for any other
# mac test run on this machine (the lock `make prepush` takes for UI tests).
ui-shots:
	@test -n "$(SCENARIO)" || { echo "usage: make ui-shots SCENARIO=mac/scenarios/<name>.json [OUT=<dir>]"; exit 2; }
	lockf -k /tmp/openlab-xcuitest.lock mac/scenarios/ui-shots.sh "$(SCENARIO)" "$(or $(OUT),build/ui-shots/$(basename $(notdir $(SCENARIO))))"

mac: project
	$(XCODEBUILD) -configuration Release build
	@echo "Built build/Build/Products/Release/openLab.app"

verify-bundle:
	mac/scripts/verify-bundle.sh build/Build/Products/Release/openLab.app

# Engine-only rebuild still gives a validly signed bundle (slow: Release builds; run in CI nightly).
test-bundle-rebuild:
	mac/scripts/check-incremental-bundle.sh

# The release disk image from the Release app (`make mac` first): build/openLab-<version>.dmg.
dmg:
	mac/scripts/make-dmg.sh build/Build/Products/Release/openLab.app build

project:
	cd mac && xcodegen --quiet
