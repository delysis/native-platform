// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "MomQwen3TTSHelper",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(url: "https://github.com/ml-explore/mlx-swift", exact: "0.30.3")
    ],
    targets: [
        .target(
            name: "Qwen3TTS",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXNN", package: "mlx-swift"),
                .product(name: "MLXRandom", package: "mlx-swift"),
                .product(name: "MLXFast", package: "mlx-swift")
            ]
        ),
        .executableTarget(name: "Qwen3TTSHelper", dependencies: [
            "Qwen3TTS",
            .product(name: "MLX", package: "mlx-swift")
        ]),
        .testTarget(name: "Qwen3TTSTests", dependencies: ["Qwen3TTS"])
    ]
)
