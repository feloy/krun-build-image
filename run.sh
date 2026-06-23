#!/bin/sh
set -e
cargo build
codesign --sign - --entitlements entitlements.plist --force target/debug/krun-build-image
DYLD_LIBRARY_PATH="$(brew --prefix)/lib:${DYLD_LIBRARY_PATH}" ./target/debug/krun-build-image "$@"
