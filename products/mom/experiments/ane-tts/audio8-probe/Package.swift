// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "mom-audio8-probe",
    platforms: [.macOS("27.0")],
    products: [.executable(name: "mom-audio8-probe", targets: ["MomAudio8Probe"])],
    dependencies: [
        .package(
            url: "https://github.com/john-rocky/coreai-kit.git",
            revision: "172127871ad2b595642664a9febb9b6eb97fb9d8"
        )
    ],
    targets: [
        .executableTarget(
            name: "MomAudio8Probe",
            dependencies: [
                .product(name: "CoreAIKit", package: "coreai-kit"),
                .product(name: "CoreAIKitVision", package: "coreai-kit"),
            ]
        )
    ]
)
