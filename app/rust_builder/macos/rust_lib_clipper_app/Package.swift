// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "rust_lib_clipper_app",
    platforms: [
        .macOS("10.11")
    ],
    products: [
        .library(name: "rust-lib-clipper-app", targets: ["rust_lib_clipper_app"])
    ],
    dependencies: [
        .package(name: "FlutterFramework", path: "../FlutterFramework")
    ],
    targets: [
        .target(
            name: "rust_lib_clipper_app",
            dependencies: [
                .product(name: "FlutterFramework", package: "FlutterFramework")
            ]
        )
    ]
)
