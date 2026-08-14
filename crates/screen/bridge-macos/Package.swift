// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "SottoScreenBridge",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "SottoScreenBridge", type: .static, targets: ["SottoScreenBridge"])
    ],
    targets: [.target(name: "SottoScreenBridge")]
)
