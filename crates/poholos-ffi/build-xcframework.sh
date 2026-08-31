#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Ivan Petrouchtchak
#
# Packages the poholos-ffi crate as an XCFramework for the iOS app:
# builds the static library for device, simulator, and the host Mac (the
# Mac slice is what lets the Swift package's tests and CLI monitor run
# on the development machine), generates the C header with cbindgen,
# bundles the Clang module map, and assembles everything under the cargo
# target directory (nothing is written into the source tree):
#
#     target/xcframework/PoholosFFI.xcframework
#
# Prerequisites (one-time):
#     rustup target add aarch64-apple-ios aarch64-apple-ios-sim \
#         aarch64-apple-darwin x86_64-apple-darwin
#     cargo install cbindgen
# plus Xcode for xcodebuild.

set -euo pipefail

CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"

for tool in cargo cbindgen xcodebuild; do
    command -v "$tool" >/dev/null || {
        echo "error: $tool not found in PATH (see prerequisites in this script's header)" >&2
        exit 1
    }
done

for target in aarch64-apple-ios aarch64-apple-ios-sim \
    aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target list --installed | grep -qx "$target" || {
        echo "error: rust target $target not installed; run: rustup target add $target" >&2
        exit 1
    }
done

# Respect a caller's CARGO_TARGET_DIR; default to the workspace target dir.
TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps \
    --manifest-path "$CRATE_DIR/Cargo.toml" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')}"
OUT_DIR="$TARGET_DIR/xcframework"
INCLUDE_DIR="$OUT_DIR/include"
XCFRAMEWORK="$OUT_DIR/PoholosFFI.xcframework"

cargo build -p poholos-ffi --release --target aarch64-apple-ios
cargo build -p poholos-ffi --release --target aarch64-apple-ios-sim
cargo build -p poholos-ffi --release --target aarch64-apple-darwin
cargo build -p poholos-ffi --release --target x86_64-apple-darwin

# The headers directory becomes the Headers/ of each slice; the module
# map beside the header is what lets Swift `import PoholosFFI`.
rm -rf "$INCLUDE_DIR"
mkdir -p "$INCLUDE_DIR"
(cd "$CRATE_DIR" && cbindgen --crate poholos-ffi --output "$INCLUDE_DIR/poholos_ffi.h")
cp "$CRATE_DIR/module.modulemap" "$INCLUDE_DIR/"

# The Mac slice is universal: Swift builds for the machine's real
# architecture, which need not match the Rust toolchain's host (an
# x86_64 rustup under Rosetta on an arm64 Mac is common), so a
# single-arch slice risks being silently skipped by SPM.
lipo -create \
    "$TARGET_DIR/aarch64-apple-darwin/release/libpoholos_ffi.a" \
    "$TARGET_DIR/x86_64-apple-darwin/release/libpoholos_ffi.a" \
    -output "$OUT_DIR/libpoholos_ffi_macos.a"

# xcodebuild refuses to overwrite an existing framework.
rm -rf "$XCFRAMEWORK"
xcodebuild -create-xcframework \
    -library "$TARGET_DIR/aarch64-apple-ios/release/libpoholos_ffi.a" -headers "$INCLUDE_DIR" \
    -library "$TARGET_DIR/aarch64-apple-ios-sim/release/libpoholos_ffi.a" -headers "$INCLUDE_DIR" \
    -library "$OUT_DIR/libpoholos_ffi_macos.a" -headers "$INCLUDE_DIR" \
    -output "$XCFRAMEWORK"

echo "wrote $XCFRAMEWORK"
