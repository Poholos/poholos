// swift-tools-version:5.10
// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

import PackageDescription

let package = Package(
    name: "PoholosKit",
    platforms: [.iOS(.v16), .macOS(.v13)],
    products: [
        .library(name: "PoholosKit", targets: ["PoholosKit"]),
        .executable(name: "poholos-monitor", targets: ["poholos-monitor"]),
    ],
    targets: [
        // Produced by ./refresh-ffi.sh (gitignored); run that once after
        // cloning and after any change to the Rust engine.
        .binaryTarget(name: "PoholosFFI", path: "Frameworks/PoholosFFI.xcframework"),
        .target(name: "PoholosKit", dependencies: ["PoholosFFI"]),
        // The Mac dev loop: a terminal feed of the live mesh, so the whole
        // pipeline (scanner -> FFI -> formatting) is debuggable on the
        // development machine before a phone is ever provisioned.
        .executableTarget(name: "poholos-monitor", dependencies: ["PoholosKit"]),
        .testTarget(name: "PoholosKitTests", dependencies: ["PoholosKit"]),
    ]
)
