#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
revision=$1
mkdir -p target/tools
# The crates.io release cannot parse &raw pointers. Use a pinned upstream
# archive; cargo install --git recursively fetches unrelated fixture submodules.
curl --fail --location --silent --show-error \
    "https://github.com/mozilla/rust-code-analysis/archive/$revision.tar.gz" \
    --output target/tools/rca-source.tar.gz
tar -xzf target/tools/rca-source.tar.gz -C target/tools
cargo install --path "target/tools/rust-code-analysis-$revision/rust-code-analysis-cli" \
    --locked --root target/tools --force
touch "target/tools/.rca-$revision"
