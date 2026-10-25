# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build

.PHONY: lint test test-rust test-mac mac dmg project

lint:
	cd engine && cargo fmt --all --check
	cd engine && cargo clippy --workspace --all-targets -- -D warnings
	cd engine && cargo deny check
	swift format lint --strict -r mac/

test: test-rust test-mac

test-rust:
	cd engine && cargo test --workspace

test-mac: project
	$(XCODEBUILD) -destination 'platform=macOS' test

mac: project
	$(XCODEBUILD) -configuration Release build
	@echo "Built build/Build/Products/Release/openLab.app"

dmg:
	@echo "dmg: not yet implemented (M6.1)"; exit 1

project:
	cd mac && xcodegen --quiet
