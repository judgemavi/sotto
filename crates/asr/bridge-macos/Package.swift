// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "SottoAsrBridge",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "SottoAsrBridge", type: .static, targets: ["SottoAsrBridge"])
    ],
    targets: [
        .target(name: "SottoAsrBridge"),
        .testTarget(name: "SottoAsrBridgeTests", dependencies: ["SottoAsrBridge"]),
    ]
)
