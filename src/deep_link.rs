//! `survivorsforge://addon/<id>` links from the website: the system starts the launcher with the
//! link, and a launcher that already runs gets the addon from the new one and opens it.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

pub const SCHEME: &str = "survivorsforge";
/// Where a running launcher listens for links. Loopback only.
const PORT: u16 = 47615;
/// Said first by the running launcher, so a link is never handed to another program on the port.
const GREETING: &str = "survivorsforge 1";

/// The addon id of `survivorsforge://addon/<id>`, `None` for anything else.
pub fn addon_id(link: &str) -> Option<String> {
    let rest = link.trim().strip_prefix(SCHEME)?.strip_prefix("://")?;
    let id = rest.strip_prefix("addon/")?.trim_end_matches('/');
    valid_id(id).then(|| id.to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn address() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, PORT))
}

/// Hands `id` to a launcher that already runs. False when none answers.
pub fn send_to_running(id: &str) -> bool {
    let send = || -> std::io::Result<bool> {
        let stream = TcpStream::connect_timeout(&address(), Duration::from_millis(500))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut greeting = String::new();
        BufReader::new(&stream).read_line(&mut greeting)?;
        if greeting.trim_end() != GREETING {
            return Ok(false);
        }
        (&stream).write_all(format!("{id}\n").as_bytes())?;
        Ok(true)
    };
    send().unwrap_or(false)
}

/// Listens for links from launchers started later and calls `open` with each addon id.
/// False when the port is taken (another launcher, or another program).
pub fn listen(open: impl Fn(String) + Send + 'static) -> bool {
    let Ok(listener) = TcpListener::bind(address()) else { return false };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if (&stream).write_all(format!("{GREETING}\n").as_bytes()).is_err() {
                continue;
            }
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_ok() && valid_id(line.trim_end()) {
                open(line.trim_end().to_string());
            }
        }
    });
    true
}

/// Lets the system open `survivorsforge://` links with this executable. Per user, no admin rights;
/// written on every start, so the link follows the executable when it moves.
pub fn register() {
    #[cfg(any(windows, target_os = "linux"))]
    let Ok(exe) = std::env::current_exe() else { return };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let key = format!(r"HKCU\Software\Classes\{SCHEME}");
        let command = format!("\"{}\" \"%1\"", exe.display());
        for args in [
            vec!["add", key.as_str(), "/ve", "/d", "URL:SurvivorsForge", "/f"],
            vec!["add", key.as_str(), "/v", "URL Protocol", "/d", "", "/f"],
            vec!["add", &format!(r"{key}\shell\open\command"), "/ve", "/d", command.as_str(), "/f"],
        ] {
            let _ = std::process::Command::new("reg").args(&args).creation_flags(CREATE_NO_WINDOW).output();
        }
    }
    #[cfg(target_os = "linux")]
    {
        // An AppImage runs from a mount that changes; the system must start the AppImage itself.
        let exe = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from).unwrap_or(exe);
        let Some(dirs) = directories::BaseDirs::new() else { return };
        let apps = dirs.data_dir().join("applications");
        let file = "survivorsforge-link.desktop";
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=SurvivorsForge\nExec=\"{}\" %u\nMimeType=x-scheme-handler/{SCHEME};\nNoDisplay=true\n",
            exe.display()
        );
        if std::fs::create_dir_all(&apps).is_ok() && std::fs::write(apps.join(file), entry).is_ok() {
            let _ = std::process::Command::new("xdg-mime").args(["default", file, &format!("x-scheme-handler/{SCHEME}")]).output();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_name_one_addon() {
        assert_eq!(addon_id("survivorsforge://addon/survivor-ui").as_deref(), Some("survivor-ui"));
        assert_eq!(addon_id("survivorsforge://addon/lucky/").as_deref(), Some("lucky"), "browsers may add a slash");
        assert_eq!(addon_id("survivorsforge://addon/"), None);
        assert_eq!(addon_id("survivorsforge://addon/a b"), None);
        assert_eq!(addon_id("survivorsforge://addon/../x"), None, "no path tricks");
        assert_eq!(addon_id("survivorsforge://pack/x"), None);
        assert_eq!(addon_id("https://addon/x"), None);
        assert_eq!(addon_id(r"C:\some\file.zip"), None, "a normal argument");
    }
}
