# SurvivorsForge on macOS

Download `SurvivorsForge-macos-arm64.zip` from the [GitHub release](https://github.com/Rezondes/SurvivorsForge/releases/latest)
and extract `SurvivorsForge.app`. It requires an Apple Silicon Mac with macOS 26.2
or later and Rosetta 2. The release app has an ad hoc signature and no Apple notarization,
so macOS may ask you to [approve it on first open](https://support.apple.com/en-gb/102445).

Open `SurvivorsForge.app`.
In setup, select the game's `benilla-config/AddOns` folder. The game folder above it
must contain `AzerothSurvivors.exe`. Use **Launch game** to start that copy.

SurvivorsForge bundles Wine and creates its own prefix at
`~/Library/Application Support/SurvivorsForge/WinePrefix` on first launch. It uses
the selected game folder for the executable, game data and saves. If the game is
already open, close it before starting it or applying a game update from the launcher.

## Build locally

From the repository root on Apple Silicon, with stable Rust and the Xcode Command Line Tools:

```sh
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p survivorsforge
bash packaging/macos/build-app.sh target/release/SurvivorsForge 0.6.3 dist
```

The script downloads and verifies the standalone Wine runtime, then creates an
ad hoc signed `dist/SurvivorsForge.app`. An optional fourth argument supplies
the same runtime archive from disk.

## Wine runtime

The bundle uses the unmodified [Wine runtime r17](https://github.com/WoWSilicon/WoWSilicon/releases/tag/wine-runtime-r17)
from WoWSilicon 3.2.2. The runtime contains Wine 11.13, MoltenVK and its support libraries.
The standalone runtime is bundled inside SurvivorsForge, so WoWSilicon does not need
to be installed.
The [source and runtime build instructions](https://github.com/WoWSilicon/WoWSilicon/tree/v3.2.2/tools/wine-runtime)
and the [runtime source lock](https://github.com/WoWSilicon/WoWSilicon/blob/v3.2.2/Packaging/WineRuntime/runtime-lock.json)
identify the Wine fork, patches and overlays used in the bundle.
