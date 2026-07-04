#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Ivan Petrouchtchak
#
# Rebuilds the Rust engine and installs the resulting XCFramework where
# Package.swift expects it (Frameworks/, gitignored). Run once after
# cloning and after any change to the Rust side.

set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$APP_DIR/../.." && pwd)"

"$REPO_DIR/crates/poholos-ffi/build-xcframework.sh"

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_DIR/target}"
mkdir -p "$APP_DIR/Frameworks"
rm -rf "$APP_DIR/Frameworks/PoholosFFI.xcframework"
cp -R "$TARGET_DIR/xcframework/PoholosFFI.xcframework" "$APP_DIR/Frameworks/"

echo "installed $APP_DIR/Frameworks/PoholosFFI.xcframework"
