// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MomQwen3TTSANEEvaluation",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "qwen3-tts-ane-eval", targets: ["QwenANEEval"]),
    ],
    targets: [
        .target(
            name: "QwenANE",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                .linkedFramework("Accelerate"),
                .linkedFramework("CoreML"),
            ]
        ),
        .executableTarget(
            name: "QwenANEEval",
            dependencies: ["QwenANE"],
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                .linkedFramework("Accelerate"),
                .linkedFramework("CoreML"),
            ]
        ),
        .testTarget(
            name: "QwenANEEvalTests",
            dependencies: ["QwenANE"],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
