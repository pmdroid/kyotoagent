// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "KyotoAgent",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [
        .library(name: "KyotoAgent", targets: ["KyotoAgent"])
    ],
    targets: [
        .target(name: "KyotoAgent"),
        .testTarget(
            name: "KyotoAgentTests",
            dependencies: ["KyotoAgent"],
            resources: [
                .copy("Bodies")
            ]
        )
    ]
)
