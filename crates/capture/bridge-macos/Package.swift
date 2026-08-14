// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "SottoCaptureBridge",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "SottoCaptureBridge", type: .static, targets: ["SottoCaptureBridge"])
    ],
    targets: [
        .target(name: "SottoCaptureBridge"),
        .testTarget(name: "SottoCaptureBridgeTests", dependencies: ["SottoCaptureBridge"]),
    ]
)
