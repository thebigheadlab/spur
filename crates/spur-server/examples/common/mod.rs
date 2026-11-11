//! Shared by the examples: a small key layer and a protocol client over a Unix socket.
//!
//! The key layer is example-only. The server ships no bindings; UX packs bring them.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use spur_rpc::{Caps, ClientEnd, ClientKind, ClientMsg, ServerMsg, Size, SurfaceView};
use spur_server::Session;

/// Insert-mode typing, arrows, Enter, Backspace and undo.
pub const KEYS: &str = r#"
[mode.insert]
fallback = "editor.insert_text"

[[bind]]
mode = "insert"
keys = "<Left>"
command = "editor.move_left"

[[bind]]
mode = "insert"
keys = "<Right>"
command = "editor.move_right"

[[bind]]
mode = "insert"
keys = "<Down>"
command = "editor.move_down"

[[bind]]
mode = "insert"
keys = "<Up>"
command = "editor.move_up"

[[bind]]
mode = "insert"
keys = "<CR>"
command = "editor.insert_newline"

[[bind]]
mode = "insert"
keys = "<BS>"
command = "editor.delete_backward"

[[bind]]
mode = "insert"
keys = "<C-z>"
command = "editor.undo"
"#;

const WAIT: Duration = Duration::from_secs(10);

/// A frontend that keeps its latest surface and acks every flush.
pub struct Client {
    end: ClientEnd,
    /// The latest surface frame.
    pub view: Option<SurfaceView>,
    /// The id the server gave this client.
    pub id: u64,
    ping: u64,
}

impl Client {
    /// Connects over a fresh socket pair and says hello.
    ///
    /// # Panics
    ///
    /// If the socket or the handshake fails (the examples stop there).
    pub fn connect(session: &Session, name: &str, size: Size) -> Self {
        let (mine, theirs) = UnixStream::pair().expect("socket pair");
        session.attach_unix(theirs).expect("attach");
        let mut c = Self {
            end: ClientEnd::unix(mine).expect("client end"),
            view: None,
            id: 0,
            ping: 0,
        };
        c.end
            .send(ClientMsg::Hello {
                protocols: vec![spur_rpc::PROTOCOL_VERSION],
                kind: ClientKind::Frontend,
                name: name.into(),
                caps: Caps::default(),
                size,
            })
            .expect("hello");
        c.sync();
        c
    }

    /// Sends keys and waits until the frame they caused has arrived.
    pub fn keys(&mut self, keys: &str) {
        self.ping += 1;
        let key = ClientMsg::Key { key: keys.into() };
        let ping = ClientMsg::Ping { id: self.ping };
        self.end.send_batch(vec![key, ping]).expect("send");
        self.wait();
    }

    /// Waits until the server has sent everything due to this client.
    pub fn sync(&mut self) {
        self.ping += 1;
        self.end
            .send(ClientMsg::Ping { id: self.ping })
            .expect("send");
        self.wait();
    }

    fn wait(&mut self) {
        loop {
            match self.end.recv_timeout(WAIT) {
                Ok(Some(ServerMsg::Pong { id, .. })) if id == self.ping => return,
                Ok(Some(m)) => self.take(m),
                other => panic!("no pong: {other:?}"),
            }
        }
    }

    fn take(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Welcome { client, .. } => self.id = client,
            ServerMsg::Surface { view, .. } => self.view = Some(view),
            ServerMsg::Flush { frame } => {
                let _ = self.end.send(ClientMsg::Ack { frame });
            }
            _ => {}
        }
    }

    /// Reads and acks until the socket closes (for a client nobody drives).
    #[allow(dead_code, reason = "each example uses part of this module")]
    pub fn serve(mut self) {
        while let Ok(m) = self.end.recv() {
            self.take(m);
        }
    }
}
