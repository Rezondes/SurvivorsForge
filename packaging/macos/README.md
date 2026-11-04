# SurvivorsForge on macOS

Download `SurvivorsForge-macos-arm64.zip` from the [GitHub release](https://github.com/Rezondes/SurvivorsForge/releases/latest)
and extract `SurvivorsForge.app`. It requires an Apple Silicon Mac with macOS 26.2
or later and Rosetta 2. The release app has an ad hoc signature and no Apple notarization,
so macOS may ask you to [approve it on first open](https://support.apple.com/en-gb/102445).

Open `SurvivorsForge.app`. In setup, select the game's `benilla-config/AddOns` folder. The game folder above it
must contain `AzerothSurvivors.exe`. Use **Launch game** to start that copy.
Use **Browse** in the Mac app to install addons. Website install links work on
Windows and Linux.

SurvivorsForge bundles Wine and creates its own prefix at
`~/Library/Application Support/SurvivorsForge/WinePrefix` on first launch. It uses
the selected game folder for the executable, game data and saves. If the game is
already open, close it before starting it or applying a game update from the launcher.

## Build locally

From the repository root on Apple Silicon, with stable Rust and the Xcode Command Line Tools:

```sh
MACOSX_DEPLOYMENT_TARGET=26.2 cargo build --locked --release -p survivorsforge
bash packaging/macos/build-app.sh target/release/SurvivorsForge 0.6.3 dist
```

The script downloads and verifies the standalone Wine runtime, then creates an
ad hoc signed `dist/SurvivorsForge.app`. An optional fourth argument supplies
the same runtime archive from disk.

The standalone Wine runtime is bundled with the app. See the [third-party
notices](THIRD-PARTY-NOTICES.md) for the runtime source and licenses.
