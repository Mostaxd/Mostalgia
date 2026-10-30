//! The channel `mostalgia toggle` and friends use to reach the running
//! desktop app where there are no Unix sockets (Windows): a TCP port on
//! 127.0.0.1, announced in a file in the data directory together with a
//! random token. Only someone who can read that file (the user) can send
//! commands; a web page poking at localhost can't.
//!
//! Protocol: one line `<token> <command>\n`, answered with one line
//! (`ok`, or an error) after which the connection closes. `ping` is
//! answered by the channel itself, to tell a running app from a stale file.
#![cfg_attr(unix, allow(dead_code))]

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// Where the running app announces its port and token.
pub fn endpoint_file() -> PathBuf {
    crate::paths::data_dir().join("control.txt")
}

/// A fresh random token (128 bits from the OS-seeded hasher keys).
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn read_endpoint(file: &Path) -> std::io::Result<(u16, String)> {
    let text = std::fs::read_to_string(file)?;
    let mut parts = text.split_whitespace();
    let port = parts.next().and_then(|p| p.parse().ok());
    let token = parts.next();
    match (port, token) {
        (Some(port), Some(token)) => Ok((port, token.to_string())),
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed control file")),
    }
}

/// Sends `command` to the app announced in `file`; returns its reply line.
pub fn send(file: &Path, command: &str) -> std::io::Result<String> {
    let (port, token) = read_endpoint(file)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("{token} {}\n", command.trim()).as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(reply.trim().to_string())
}

/// Whether an app is listening at `file`'s endpoint.
pub fn is_running(file: &Path) -> bool {
    send(file, "ping").is_ok_and(|r| r == "ok")
}

/// Starts listening on a background thread and announces the endpoint in
/// `file`. `handle` gets each command and says whether it knew it. Fails
/// with `AddrInUse` if another app already listens there.
pub fn serve<F>(file: &Path, handle: F) -> std::io::Result<()>
where
    F: Fn(&str) -> bool + Send + 'static,
{
    if is_running(file) {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "Mostalgia is already running"));
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written whole, then moved into place, so a reader never sees half.
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, format!("{port} {token}\n"))?;
    std::fs::rename(&tmp, file)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(TIMEOUT));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let reply = match line.trim().split_once(' ') {
                Some((t, _)) if t != token => "unauthorized",
                Some((_, "ping")) => "ok",
                Some((_, cmd)) if handle(cmd) => "ok",
                Some(_) => "unknown command",
                None => "unauthorized",
            };
            let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
        }
    });
    Ok(())
}

/// Removes the announcement (when the app quits).
pub fn cleanup(file: &Path) {
    let _ = std::fs::remove_file(file);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn commands_reach_the_app_with_the_token_only() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mostalgia-control-{}", std::process::id()));
        let file = dir.join("control.txt");
        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let got = got.clone();
            serve(&file, move |cmd| {
                got.lock().unwrap().push(cmd.to_string());
                cmd == "toggle"
            })?;
        }
        assert!(is_running(&file));
        assert_eq!(send(&file, "toggle")?, "ok");
        assert_eq!(send(&file, "dance")?, "unknown command");
        assert_eq!(*got.lock().unwrap(), ["toggle", "dance"]);

        // A second app sees the first and stays out.
        let second = serve(&file, |_| true).unwrap_err();
        assert_eq!(second.kind(), std::io::ErrorKind::AddrInUse);

        // Without the token: refused, nothing handled.
        let (port, _) = read_endpoint(&file)?;
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        s.write_all(b"guess toggle\n")?;
        let mut reply = String::new();
        BufReader::new(&s).read_line(&mut reply)?;
        assert_eq!(reply.trim(), "unauthorized");
        assert_eq!(got.lock().unwrap().len(), 2);

        // A stale file (nobody listening) doesn't count as running.
        std::fs::write(&file, "1 deadbeef\n")?;
        assert!(!is_running(&file));
        assert!(send(&file, "toggle").is_err());
        cleanup(&file);
        assert!(!file.exists());
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn tokens_differ() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
