.DEFAULT_GOAL := check
RCA_REV := 37e5d83c056c8cbf827223d5814a93c5218df1a9
COVERAGE_MIN_LINES := 75

.PHONY: check fmt fmt-check lint test coverage package security complexity tools

check: fmt-check lint test complexity

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --locked --all-targets --all-features -- -D warnings

test:
	cargo test --locked --all-targets --all-features

coverage:
	cargo llvm-cov --locked --all-targets --all-features --fail-under-lines $(COVERAGE_MIN_LINES)

package:
	cargo package --locked --allow-dirty

security:
	cargo audit --deny warnings

complexity: tools
	python3 scripts/complexity.py

tools:
	@if ! test -x target/tools/bin/rust-code-analysis-cli || ! test -f target/tools/.rca-$(RCA_REV); then \
		sh scripts/install-tools.sh $(RCA_REV); \
	fi
