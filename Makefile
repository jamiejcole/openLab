# SPDX-License-Identifier: GPL-3.0-or-later
XCODEBUILD := xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build
SPDX := SPDX-License-Identifier: GPL-3.0-or-later

.PHONY: lint test test-rust test-mac mac verify-bundle dmg project

lint:
	@missing=$$(git ls-files -co --exclude-standard '*.rs' '*.swift' '*.sh' '*.yml' Makefile | xargs grep -L "$(SPDX)"); \
		if [ -n "$$missing" ]; then echo "Missing SPDX header:"; echo "$$missing"; exit 1; fi
	cd engine && cargo fmt --all --check
	cd engine && cargo clippy --workspace --all-targets -- -D warnings
	cd engine && cargo deny check
	swift format lint --strict -r mac/

test: test-rust test-mac

test-rust:
	cd engine && cargo test --workspace

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
