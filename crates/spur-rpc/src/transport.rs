//! Transports. In-process connections move messages through channels; stream
//! connections (sockets, pipes) carry length-prefixed JSON frames: a 4-byte
//! little-endian length, then the JSON body.
//!
//! Messages travel in batches. A batch is written to a stream with one `write`, and
//! frames that arrive together are handed on together, so one keystroke's reply
//! (`surface`, `flush`, `pong`) costs one syscall and one wakeup on each side.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::protocol::{ClientMsg, ServerMsg};

/// Largest accepted frame body, in bytes. A peer that sends more is disconnected.
pub const MAX_FRAME: usize = 16 << 20;

/// The other side of a connection is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("connection closed")
    }
}

impl std::error::Error for Closed {}

/// One side of a connection, sending `S` and receiving `R`. Dropping it closes the
/// connection for the other side.
#[derive(Debug)]
pub struct Endpoint<S, R> {
    tx: Sender<Vec<S>>,
    rx: Receiver<Vec<R>>,
    pending: VecDeque<R>,
}

/// A client's side: sends [`ClientMsg`], receives [`ServerMsg`].
pub type ClientEnd = Endpoint<ClientMsg, ServerMsg>;
/// A server's side: sends [`ServerMsg`], receives [`ClientMsg`].
pub type ServerEnd = Endpoint<ServerMsg, ClientMsg>;

impl<S, R> Endpoint<S, R> {
    fn new(tx: Sender<Vec<S>>, rx: Receiver<Vec<R>>) -> Self {
        Self {
            tx,
            rx,
            pending: VecDeque::new(),
        }
    }

    /// Sends one message.
    ///
    /// # Errors
    ///
    /// [`Closed`] if the other side is gone.
    pub fn send(&self, msg: S) -> Result<(), Closed> {
        self.send_batch(vec![msg])
    }

    /// Sends several messages as one batch (one write on a stream).
    ///
    /// # Errors
    ///
    /// [`Closed`] if the other side is gone.
    pub fn send_batch(&self, msgs: Vec<S>) -> Result<(), Closed> {
        if msgs.is_empty() {
            return Ok(());
        }
        self.tx.send(msgs).map_err(|_| Closed)
    }

    /// Waits for the next message.
    ///
    /// # Errors
    ///
    /// [`Closed`] once the other side is gone and nothing is left to read.
    pub fn recv(&mut self) -> Result<R, Closed> {
        loop {
            if let Some(m) = self.pending.pop_front() {
                return Ok(m);
            }
            self.pending.extend(self.rx.recv().map_err(|_| Closed)?);
        }
    }

    /// Waits up to `timeout`; `Ok(None)` if nothing arrived in time.
    ///
    /// # Errors
    ///
    /// [`Closed`] once the other side is gone and nothing is left to read.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<R>, Closed> {
        if let Some(m) = self.pending.pop_front() {
            return Ok(Some(m));
        }
        match self.rx.recv_timeout(timeout) {
            Ok(batch) => {
                self.pending.extend(batch);
                Ok(self.pending.pop_front())
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(Closed),
        }
    }

    /// Returns a message if one is waiting.
    ///
    /// # Errors
    ///
    /// [`Closed`] once the other side is gone and nothing is left to read.
    pub fn try_recv(&mut self) -> Result<Option<R>, Closed> {
        if let Some(m) = self.pending.pop_front() {
            return Ok(Some(m));
        }
        match self.rx.try_recv() {
            Ok(batch) => {
                self.pending.extend(batch);
                Ok(self.pending.pop_front())
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(Closed),
        }
    }

    /// Splits into the batch sender and receiver (messages already received but not yet
    /// read are dropped).
    pub fn split(self) -> (Sender<Vec<S>>, Receiver<Vec<R>>) {
        (self.tx, self.rx)
    }
}

/// A connected pair inside one process. Messages are moved, not serialized.
pub fn in_process() -> (ClientEnd, ServerEnd) {
    let (c2s_tx, c2s_rx) = mpsc::channel();
    let (s2c_tx, s2c_rx) = mpsc::channel();
    (Endpoint::new(c2s_tx, s2c_rx), Endpoint::new(s2c_tx, c2s_rx))
}

impl ClientEnd {
    /// A client endpoint over a byte stream (a socket's two halves, a pair of pipes).
    /// Two threads move frames; they stop when the stream closes or the endpoint is
    /// dropped. A malformed or oversized frame from the server closes the connection.
    ///
    /// # Panics
    ///
    /// If the OS cannot start a thread.
    pub fn framed<Rd, Wr>(reader: Rd, writer: Wr) -> Self
    where
        Rd: Read + Send + 'static,
        Wr: Write + Send + 'static,
    {
        Self::framed_closing(reader, writer, || {})
    }

    /// Like [`Self::framed`], with `close` run once the endpoint is dropped and its
    /// last batch is written. Use it to shut a socket down, since the reader thread's
    /// half keeps the socket open otherwise.
    ///
    /// # Panics
    ///
    /// If the OS cannot start a thread.
    pub fn framed_closing<Rd, Wr, F>(reader: Rd, writer: Wr, close: F) -> Self
    where
        Rd: Read + Send + 'static,
        Wr: Write + Send + 'static,
        F: FnOnce() + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<Vec<ClientMsg>>();
        let (in_tx, in_rx) = mpsc::channel::<Vec<ServerMsg>>();
        std::thread::Builder::new()
            .name("spur-rpc-read".into())
            .spawn(move || {
                let mut frames = FrameReader::new(reader);
                while let Ok(Some(batch)) = frames.read_batch::<ServerMsg>() {
                    if in_tx.send(batch).is_err() {
                        break;
                    }
                }
            })
            .unwrap_or_else(|e| panic!("cannot start a thread: {e}"));
        std::thread::Builder::new()
            .name("spur-rpc-write".into())
            .spawn(move || {
                pump_writes(&out_rx, writer);
                close();
            })
            .unwrap_or_else(|e| panic!("cannot start a thread: {e}"));
        Self::new(out_tx, in_rx)
    }

    /// A client endpoint over a Unix socket, shut down when the endpoint is dropped.
    ///
    /// # Errors
    ///
    /// If the socket cannot be cloned.
    #[cfg(unix)]
    pub fn unix(stream: std::os::unix::net::UnixStream) -> io::Result<Self> {
        let reader = stream.try_clone()?;
        let closer = stream.try_clone()?;
        Ok(Self::framed_closing(reader, stream, move || {
            let _ = closer.shutdown(std::net::Shutdown::Both);
        }))
    }
}

/// Writes every batch from `rx` to `writer`, merging batches that are already waiting
/// into one write. Returns when `rx` closes or a write fails.
pub fn pump_writes<T: Serialize, W: Write>(rx: &Receiver<Vec<T>>, writer: W) {
    let mut frames = FrameWriter::new(writer);
    while let Ok(batch) = rx.recv() {
        let mut ok = batch.iter().all(|m| frames.push(m).is_ok());
        while let Ok(more) = rx.try_recv() {
            ok &= more.iter().all(|m| frames.push(m).is_ok());
        }
        if !ok || frames.flush().is_err() {
            return;
        }
    }
}

/// Encodes frames into one buffer and writes them with one `write_all`.
#[derive(Debug)]
pub struct FrameWriter<W> {
    writer: W,
    buf: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    /// Wraps a writer.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            buf: Vec::with_capacity(4096),
        }
    }

    /// Appends one frame to the buffer.
    ///
    /// # Errors
    ///
    /// `InvalidData` if the message does not serialize or is larger than
    /// [`MAX_FRAME`]; the buffer is left as it was.
    pub fn push<T: Serialize>(&mut self, msg: &T) -> io::Result<()> {
        let start = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        if let Err(e) = serde_json::to_writer(&mut self.buf, msg) {
            self.buf.truncate(start);
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
        let len = self.buf.len() - start - 4;
        let Some(len32) = u32::try_from(len).ok().filter(|_| len <= MAX_FRAME) else {
            self.buf.truncate(start);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame too large",
            ));
        };
        self.buf[start..start + 4].copy_from_slice(&len32.to_le_bytes());
        Ok(())
    }

    /// Writes everything buffered in one `write_all`, then flushes the writer.
    ///
    /// # Errors
    ///
    /// The I/O error. The buffer is cleared either way.
    pub fn flush(&mut self) -> io::Result<()> {
        let res = self
            .writer
            .write_all(&self.buf)
            .and_then(|()| self.writer.flush());
        self.buf.clear();
        res
    }

    /// The wrapped writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

/// Writes one frame.
///
/// # Errors
///
/// The I/O error, or `InvalidData` for a message larger than [`MAX_FRAME`].
pub fn write_frame<W: Write, T: Serialize>(writer: W, msg: &T) -> io::Result<()> {
    let mut frames = FrameWriter::new(writer);
    frames.push(msg)?;
    frames.flush()
}

/// Reads frames from a byte stream.
#[derive(Debug)]
pub struct FrameReader<R> {
    reader: BufReader<R>,
}

impl<R: Read> FrameReader<R> {
    /// Wraps a reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(64 << 10, reader),
        }
    }

    /// Reads one frame body as raw bytes. `Ok(None)` means the stream ended cleanly
    /// between frames.
    ///
    /// # Errors
    ///
    /// The I/O error, `UnexpectedEof` inside a frame, or `InvalidData` for a length
    /// over [`MAX_FRAME`] (checked before anything is allocated).
    pub fn read_body(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        let mut got = 0;
        while got < len.len() {
            match self.reader.read(&mut len[got..]) {
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
        self.reader.read_exact(&mut body)?;
        Ok(Some(body))
    }

    /// Reads one frame. `Ok(None)` means the stream ended cleanly between frames.
    ///
    /// # Errors
    ///
    /// As [`Self::read_body`], or `InvalidData` for a body that does not decode.
    pub fn read<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        match self.read_body()? {
            None => Ok(None),
            Some(body) => serde_json::from_slice(&body)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        }
    }

    /// Waits for one frame, then also takes every frame already received, so frames
    /// that arrived together are handed on together. `Ok(None)` at a clean end.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub fn read_batch<T: DeserializeOwned>(&mut self) -> io::Result<Option<Vec<T>>> {
        let Some(first) = self.read()? else {
            return Ok(None);
        };
        let mut batch = vec![first];
        while self.has_buffered() {
            match self.read()? {
                Some(m) => batch.push(m),
                None => break,
            }
        }
        Ok(Some(batch))
    }

    /// True if bytes of a next frame are already buffered.
    pub fn has_buffered(&self) -> bool {
        !self.reader.buffer().is_empty()
    }

    /// The buffered reader, for callers that check what is waiting.
    pub fn buf_reader(&mut self) -> &mut impl BufRead {
        &mut self.reader
    }
}

/// Reads one frame from `reader` (unbuffered convenience for tests and tools).
///
/// # Errors
///
/// As [`FrameReader::read`].
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: R) -> io::Result<Option<T>> {
    FrameReader::new(reader).read()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Caps, ClientKind, Size};

    fn hello() -> ClientMsg {
        ClientMsg::Hello {
            protocols: vec![0],
            kind: ClientKind::Frontend,
            name: "t".into(),
            caps: Caps::default(),
            size: Size { cols: 80, rows: 24 },
        }
    }

    #[test]
    fn in_process_pair_talks_both_ways() {
        let (mut c, mut s) = in_process();
        c.send_batch(vec![hello(), ClientMsg::Bye]).unwrap();
        assert_eq!(s.recv(), Ok(hello()));
        assert_eq!(s.try_recv(), Ok(Some(ClientMsg::Bye)));
        assert_eq!(s.try_recv(), Ok(None));
        s.send(ServerMsg::Flush { frame: 1 }).unwrap();
        assert_eq!(
            c.recv_timeout(Duration::from_secs(1)),
            Ok(Some(ServerMsg::Flush { frame: 1 }))
        );
        drop(s);
        assert_eq!(c.recv(), Err(Closed));
        assert_eq!(c.send(ClientMsg::Bye), Err(Closed));
    }

    #[test]
    fn frames_round_trip_and_reject_garbage() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &hello()).unwrap();
        write_frame(&mut buf, &ClientMsg::Bye).unwrap();
        let mut r = FrameReader::new(&buf[..]);
        assert_eq!(r.read::<ClientMsg>().unwrap(), Some(hello()));
        assert_eq!(r.read::<ClientMsg>().unwrap(), Some(ClientMsg::Bye));
        assert_eq!(r.read::<ClientMsg>().unwrap(), None);

        // A length over the limit is refused before anything is allocated.
        let huge = u32::MAX.to_le_bytes();
        assert!(read_frame::<_, ClientMsg>(&huge[..]).is_err());
        // Truncated body, non-JSON body, truncated length.
        assert!(read_frame::<_, ClientMsg>(&[5u8, 0, 0, 0, b'{'][..]).is_err());
        assert!(read_frame::<_, ClientMsg>(&[2u8, 0, 0, 0, b'h', b'i'][..]).is_err());
        assert!(read_frame::<_, ClientMsg>(&[1u8, 0][..]).is_err());
    }

    /// Counts `write` calls.
    struct Counting(Vec<u8>, usize);

    impl Write for Counting {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.1 += 1;
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_batch_is_one_write_and_reads_back_as_one_batch() {
        let mut w = FrameWriter::new(Counting(Vec::new(), 0));
        for m in [hello(), ClientMsg::Ping { id: 1 }, ClientMsg::Bye] {
            w.push(&m).unwrap();
        }
        w.flush().unwrap();
        assert_eq!(w.get_mut().1, 1);
        let bytes = w.get_mut().0.clone();
        let mut r = FrameReader::new(&bytes[..]);
        let batch = r.read_batch::<ClientMsg>().unwrap().unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(r.read_batch::<ClientMsg>().unwrap(), None);
    }

    fn server_echo(mut frames: FrameReader<impl Read>, out: impl Write) {
        let mut w = FrameWriter::new(out);
        while let Ok(Some(batch)) = frames.read_batch::<ClientMsg>() {
            for m in batch {
                if let ClientMsg::Ping { id } = m {
                    w.push(&ServerMsg::Pong { id, frame: 0 }).unwrap();
                }
            }
            w.flush().unwrap();
        }
    }

    #[test]
    fn framed_client_over_pipes() {
        let (c2s_r, c2s_w) = io::pipe().unwrap();
        let (s2c_r, s2c_w) = io::pipe().unwrap();
        let mut client = ClientEnd::framed(s2c_r, c2s_w);
        let server = std::thread::spawn(move || server_echo(FrameReader::new(c2s_r), s2c_w));
        client
            .send_batch(vec![ClientMsg::Ping { id: 1 }, ClientMsg::Ping { id: 2 }])
            .unwrap();
        assert_eq!(client.recv(), Ok(ServerMsg::Pong { id: 1, frame: 0 }));
        assert_eq!(client.recv(), Ok(ServerMsg::Pong { id: 2, frame: 0 }));
        drop(client);
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn framed_client_over_a_unix_socket_pair() {
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().unwrap();
        let mut client = ClientEnd::unix(a).unwrap();
        let server = std::thread::spawn(move || {
            server_echo(FrameReader::new(b.try_clone().unwrap()), b);
        });
        client.send(ClientMsg::Ping { id: 9 }).unwrap();
        assert_eq!(client.recv(), Ok(ServerMsg::Pong { id: 9, frame: 0 }));
        drop(client);
        server.join().unwrap();
    }
}
