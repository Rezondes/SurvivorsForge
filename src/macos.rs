use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Child, Command};

use survivorsforge::game_update::GAME_EXE;

fn game_name(path: &str) -> bool {
    path.trim().rsplit(['/', '\\']).next().is_some_and(|name| name.eq_ignore_ascii_case(GAME_EXE))
}

fn wine_game(line: &str) -> bool {
    let Some((host, command)) = line.trim_start().split_once(char::is_whitespace) else {
        return false;
    };
    if !matches!(host, "wine" | "wine64" | "wine-preloader" | "wine64-preloader" | "wineloader" | "wineloader2")
        && !host.eq_ignore_ascii_case("AzerothSurvivors")
    {
        return false;
    }
    // ps renders argv as text. Check the first Windows executable, so a later argument naming
    // the game does not make an unrelated Wine program count as the game.
    command
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .find(|arg| arg.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("exe")))
        .is_some_and(game_name)
}

fn process_list(columns: &str) -> io::Result<String> {
    let processes = Command::new("/bin/ps").args(["-axww", "-o", columns]).output()?;
    if !processes.status.success() {
        return Err(io::Error::other("Could not check whether Azeroth Survivors is running."));
    }
    Ok(String::from_utf8_lossy(&processes.stdout).into_owned())
}

/// Also catch a game launched outside SurvivorsForge or before it was reopened.
pub fn require_game_closed() -> io::Result<()> {
    if process_list("comm=")?.lines().any(game_name)
        || process_list("ucomm=,command=")?.lines().any(wine_game)
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_game_processes_and_wine_preloaders() {
        assert!(game_name("/Games/Azeroth Survivors/AzerothSurvivors.exe"));
        assert!(wine_game(r#"wine64-preloader "Z:\Games\Azeroth Survivors\AzerothSurvivors.exe""#));
        assert!(wine_game("wine /Applications/Wine/bin/wine /Games/Azeroth Survivors/AZEROTHSURVIVORS.EXE"));
        assert!(wine_game("AzerothSurvivors /Games/Azeroth Survivors/AzerothSurvivors.exe"));
        assert!(!wine_game("vim /Games/AzerothSurvivors.exe"));
        assert!(!wine_game("wine cmd.exe /c echo AzerothSurvivors.exe"));
        assert!(!wine_game("wine /Games/not-AzerothSurvivors.exe"));
    }
}
