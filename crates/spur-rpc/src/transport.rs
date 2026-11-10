//! Transports. An [`Endpoint`] is one side of a connection: a sender and a receiver of
//! messages. In-process connections pass the message enums through channels; stream
//! connections (sockets, pipes) pass length-prefixed JSON frames.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::protocol::{ClientMsg, ServerMsg};

/// Largest accepted frame, in bytes. A peer that sends more is disconnected.
pub const MAX_FRAME: usize = 16 << 20;

/// One side of a connection, sending `S` and receiving `R`. Dropping it closes the
/// connection for the other side.
#[derive(Debug)]
pub struct Endpoint<S, R> {
    tx: Sender<S>,
    rx: Receiver<R>,
}

/// The client's side.
pub type ClientEnd = Endpoint<ClientMsg, ServerMsg>;
/// The server's side.
pub type ServerEnd = Endpoint<ServerMsg, ClientMsg>;

impl<S, R> Endpoint<S, R> {
    /// Sends a message. Returns `false` if the other side is gone.
    pub fn send(&self, msg: S) -> bool {
        self.tx.send(msg).is_ok()
    }

    /// Waits for the next message. `None` means the other side is gone.
    pub fn recv(&self) -> Option<R> {
        self.rx.recv().ok()
    }

    /// Waits up to `timeout`. `Ok(None)` means nothing arrived in time.
    ///
    /// # Errors
    ///
    /// Returns `Err(())` if the other side is gone.
    #[allow(clippy::result_unit_err)]
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<R>, ()> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Ok(Some(m)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(()),
        }
    }

    /// Returns a message if one is waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err(())` if the other side is gone.
    #[allow(clippy::result_unit_err)]
    pub fn try_recv(&self) -> Result<Option<R>, ()> {
        match self.rx.try_recv() {
            Ok(m) => Ok(Some(m)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    /// Splits into the sender and the receiver.
    pub fn split(self) -> (Sender<S>, Receiver<R>) {
        (self.tx, self.rx)
    }
}

/// A connected pair inside one process. Messages are moved, not serialized.
pub fn in_process() -> (ClientEnd, ServerEnd) {
    let (ctx, srx) = mpsc::channel();
    let (stx, crx) = mpsc::channel();
    (Endpoint { tx: ctx, rx: crx }, Endpoint { tx: stx, rx: srx })
}

/// Writes one frame: a 4-byte little-endian length, then JSON.
///
/// # Errors
///
/// Returns the I/O error, or `InvalidData` if the message is larger than [`MAX_FRAME`].
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|_| body.len() <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Reads one frame. `Ok(None)` means the stream ended cleanly between frames.
///
/// # Errors
///
/// Returns the I/O error, or `InvalidData` for an oversized or malformed frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Turns a byte stream (a socket's two halves, a pair of pipes) into an [`Endpoint`].
/// Two threads pump frames; they stop when the stream closes or the endpoint is dropped.
/// A malformed or oversized frame closes the connection.
pub fn framed<R, W, S, T>(mut reader: R, mut writer: W) -> Endpoint<S, T>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
    S: Serialize + Send + 'static,
    T: DeserializeOwned + Send + 'static,
{
    let (out_tx, out_rx) = mpsc::channel::<S>();
    let (in_tx, in_rx) = mpsc::channel::<T>();
    std::thread::spawn(move || {
        while let Ok(Some(msg)) = read_frame(&mut reader) {
            if in_tx.send(msg).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for msg in out_rx {
            if write_frame(&mut writer, &msg).is_err() {
                break;
            }
        }
    });
    Endpoint {
        tx: out_tx,
        rx: in_rx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Caps, ClientKind, Size};

    fn hello() -> ClientMsg {
        ClientMsg::Hello {
            protocol: 0,
            kind: ClientKind::Frontend,
            name: "t".into(),
            caps: Caps::default(),
            size: Size { cols: 80, rows: 24 },
        }
    }

    #[test]
    fn in_process_pair_talks_both_ways() {
        let (c, s) = in_process();
        assert!(c.send(hello()));
        assert_eq!(s.recv(), Some(hello()));
        assert!(s.send(ServerMsg::Flush { frame: 1 }));
        assert_eq!(c.recv(), Some(ServerMsg::Flush { frame: 1 }));
        drop(s);
        assert_eq!(c.recv(), None);
        assert!(!c.send(ClientMsg::Bye));
    }

    #[test]
    fn frames_round_trip_and_reject_garbage() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &hello()).unwrap();
        write_frame(&mut buf, &ClientMsg::Bye).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame::<_, ClientMsg>(&mut r).unwrap(), Some(hello()));
        assert_eq!(
            read_frame::<_, ClientMsg>(&mut r).unwrap(),
            Some(ClientMsg::Bye)
        );
        assert_eq!(read_frame::<_, ClientMsg>(&mut r).unwrap(), None);

        // A length over the limit is refused before anything is allocated.
        let huge = u32::MAX.to_le_bytes();
        assert!(read_frame::<_, ClientMsg>(&mut &huge[..]).is_err());
        // Truncated body and non-JSON body.
        assert!(read_frame::<_, ClientMsg>(&mut &[5u8, 0, 0, 0, b'{'][..]).is_err());
        assert!(read_frame::<_, ClientMsg>(&mut &[2u8, 0, 0, 0, b'h', b'i'][..]).is_err());
        // Truncated length.
        assert!(read_frame::<_, ClientMsg>(&mut &[1u8, 0][..]).is_err());
    }

    #[test]
    fn framed_endpoints_talk_over_pipes() {
        let (c2s_r, c2s_w) = io::pipe().unwrap();
        let (s2c_r, s2c_w) = io::pipe().unwrap();
        let client: ClientEnd = framed(s2c_r, c2s_w);
        let server: ServerEnd = framed(c2s_r, s2c_w);
        assert!(client.send(hello()));
        assert_eq!(server.recv(), Some(hello()));
        assert!(server.send(ServerMsg::Bye { reason: "x".into() }));
        assert_eq!(client.recv(), Some(ServerMsg::Bye { reason: "x".into() }));
        drop(client);
        assert_eq!(server.recv(), None);
    }
}
