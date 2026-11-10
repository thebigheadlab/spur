//! Serving a session on a local socket, so clients in other processes can attach.
//!
//! The socket lives in a per-user folder with mode 0700. Unix only for now; other
//! platforms return `Unsupported` and run sessions in-process.

use std::io;
use std::path::PathBuf;

use spur_rpc::ClientEnd;

use crate::session::Attacher;

/// Checks a session name: 1 to 32 chars of `A-Z a-z 0-9 _ -`. Names become file names,
/// so nothing else is allowed.
///
/// # Errors
///
/// Returns `InvalidInput` for any other name.
pub fn check_name(name: &str) -> io::Result<()> {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a session name is 1 to 32 letters, digits, _ or -",
        ))
    }
}

/// The folder that holds session sockets: `$XDG_RUNTIME_DIR/spur`, else
/// `~/.local/state/spur/run`. Created with mode 0700.
///
/// # Errors
///
/// Returns an error if no home folder is known or the folder cannot be made private.
pub fn runtime_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no runtime or home folder"))?;
    let dir = base.join("spur").join("run");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the runtime folder is a link",
            ));
        }
        // Fails if someone else owns the folder, which is what we want.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn socket_path(name: &str) -> io::Result<PathBuf> {
    check_name(name)?;
    Ok(runtime_dir()?.join(format!("{name}.sock")))
}

/// A listening session socket. Dropping it removes the socket file.
#[derive(Debug)]
pub struct Listener {
    path: PathBuf,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::{UnixListener, UnixStream};

    use spur_rpc::{ClientEnd, ServerEnd, framed};

    use std::path::{Path, PathBuf};

    use super::{Attacher, Listener};

    /// Write half that closes the whole socket when dropped, so the read thread ends.
    struct Closing(UnixStream);

    impl Write for Closing {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Drop for Closing {
        fn drop(&mut self) {
            let _ = self.0.shutdown(Shutdown::Both);
        }
    }

    fn halves(stream: UnixStream) -> io::Result<(impl Read + Send + 'static, Closing)> {
        Ok((stream.try_clone()?, Closing(stream)))
    }

    pub(super) fn serve(attacher: Attacher, path: PathBuf) -> io::Result<Listener> {
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                if UnixStream::connect(&path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "a session with this name is already running",
                    ));
                }
                // A socket file nobody listens on: left over from a crash.
                std::fs::remove_file(&path)?;
                UnixListener::bind(&path)?
            }
            Err(e) => return Err(e),
        };
        std::thread::Builder::new()
            .name("spur-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let Ok((r, w)) = halves(stream) else { continue };
                    let end: ServerEnd = framed(r, w);
                    if !attacher.attach(end) {
                        break;
                    }
                }
            })?;
        Ok(Listener { path })
    }

    pub(super) fn connect(path: &Path) -> io::Result<ClientEnd> {
        let stream = UnixStream::connect(path)
            .map_err(|e| io::Error::new(e.kind(), format!("no such session to attach to ({e})")))?;
        let (r, w) = halves(stream)?;
        Ok(framed(r, w))
    }

    pub(super) fn alive(path: &Path) -> bool {
        UnixStream::connect(path).is_ok()
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;

    use spur_rpc::ClientEnd;

    use super::{Attacher, Listener};

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "session sockets are not available on this platform yet",
        ))
    }

    pub(super) fn serve(_: Attacher, _: std::path::PathBuf) -> io::Result<Listener> {
        unsupported()
    }

    pub(super) fn connect(_: &std::path::Path) -> io::Result<ClientEnd> {
        unsupported()
    }

    pub(super) fn alive(_: &std::path::Path) -> bool {
        false
    }
}

/// Listens for clients of a session under `name`.
///
/// # Errors
///
/// Returns `AddrInUse` if a live session already has the name, or the I/O error.
pub fn serve(attacher: Attacher, name: &str) -> io::Result<Listener> {
    imp::serve(attacher, socket_path(name)?)
}

/// Connects to the session called `name`.
///
/// # Errors
///
/// Returns an error if no such session is running.
pub fn connect(name: &str) -> io::Result<ClientEnd> {
    imp::connect(&socket_path(name)?)
}

/// Names of the sessions that are running.
///
/// # Errors
///
/// Returns an error if the runtime folder cannot be read.
pub fn list() -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(runtime_dir()?)? {
        let path = entry?.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".sock"))
        else {
            continue;
        };
        if imp::alive(&path) {
            out.push(name.to_owned());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_escape_the_runtime_folder() {
        for bad in ["", "../x", "a/b", "a b", "a.sock", &"n".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name("main").is_ok() && check_name("my-work_2").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn serves_a_session_over_a_socket() {
        use spur_rpc::{Caps, ClientKind, ClientMsg, PROTOCOL_VERSION, ServerMsg, Size};

        use crate::{Pack, Session, SessionConfig};

        let dir = std::env::temp_dir().join(format!("spur-daemon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let pack =
            "id = \"t\"\nname = \"T\"\n[layout]\nroot = \"s\"\n[node.s]\nkind = \"surface\"\n";
        let session = Session::spawn(SessionConfig {
            name: "t".into(),
            root: dir.clone(),
            pack: Pack::parse(pack).unwrap(),
            open: None,
            exit_when_empty: false,
            notice: None,
        })
        .unwrap();
        // A stale socket file from a crashed server is replaced.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let listener = imp::serve(session.attacher(), path.clone()).unwrap();
        let again = imp::serve(session.attacher(), path.clone()).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AddrInUse);
        assert!(imp::alive(&path));

        let client = imp::connect(&path).unwrap();
        client.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            kind: ClientKind::Frontend,
            name: "t".into(),
            caps: Caps::default(),
            size: Size { cols: 10, rows: 3 },
        });
        client.send(ClientMsg::Command {
            id: "editor.insert_text".into(),
            args: serde_json::json!({ "text": "over a socket" }),
        });
        let mut seen = false;
        let mut welcomed = false;
        while let Ok(Some(msg)) = client.recv_timeout(std::time::Duration::from_secs(10)) {
            match msg {
                ServerMsg::Welcome { session, .. } => welcomed = session == "t",
                ServerMsg::Surface { frame } => {
                    seen = frame.lines[0].iter().any(|s| s.text == "over a socket");
                    if seen {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(welcomed && seen);

        drop(listener);
        assert!(!path.exists(), "the socket file is removed");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
