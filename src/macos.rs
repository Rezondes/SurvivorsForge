use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Child, Command};

use survivorsforge::game_update::GAME_EXE;

/// Also catch a game launched outside SurvivorsForge or before it was reopened.
pub fn require_game_closed() -> io::Result<()> {
    let processes = Command::new("/bin/ps").args(["-axww", "-o", "comm="]).output()?;
    if !processes.status.success() {
        return Err(io::Error::other("Could not check whether Azeroth Survivors is running."));
    }
    let running = String::from_utf8_lossy(&processes.stdout).lines().any(|path| {
        path.trim()
            .rsplit(['/', '\\'])
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(GAME_EXE))
    });
    if running {
        return Err(io::Error::new(ErrorKind::AlreadyExists, "Close Azeroth Survivors first."));
    }
    Ok(())
}

pub fn start_game(exe: &Path, data_dir: &Path) -> io::Result<Child> {
    require_game_closed()?;
    let app_exe = std::env::current_exe()?;
    let runtime = app_exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Open the packaged SurvivorsForge.app."))?
        .join("Resources/Wine");
    let wine = runtime.join("bin/wine");
    if !wine.is_file() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "The bundled Wine runtime is missing. Open the packaged SurvivorsForge.app.",
        ));
    }
    std::fs::create_dir_all(data_dir)?;
    let prefix = data_dir.canonicalize()?.join("WinePrefix");
    let game_dir = exe.parent().expect("the game executable has a parent").canonicalize()?;
    let exe = game_dir.join(GAME_EXE);

    Command::new(wine)
        .arg(&exe)
        .current_dir(game_dir)
        .env("WINEPREFIX", prefix)
        .env("DYLD_LIBRARY_PATH", runtime.join("lib/external"))
        .env("VK_DRIVER_FILES", runtime.join("lib/vulkan/icd.d/MoltenVK_icd.json"))
        // Use the runtime's matching D3D9 DLL even if a different launcher left a 32-bit copy
        // beside the 64-bit game. The game doesn't need Wine's optional Mono or Gecko downloads.
        .env("WINEDLLOVERRIDES", "d3d9=b;mscoree,mshtml=")
        .env("WINE_LARGE_ADDRESS_AWARE", "1")
        .env("MVK_CONFIG_SYNCHRONOUS_QUEUE_SUBMITS", "1")
        .env_remove("WINEARCH")
        .env_remove("WINEDLLPATH")
        .env_remove("WINELOADER")
        .env_remove("WINESERVER")
        .spawn()
        .map_err(|e| {
            if e.raw_os_error() == Some(86) {
                io::Error::new(e.kind(), "The bundled Wine requires Rosetta 2. Install Rosetta and try again.")
            } else {
                e
            }
        })
}
