//! Per-client state.

use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use spur_core::editor::{Editor, ViewId};
use spur_core::{Actor, ClientId, Context, Subscription};
use spur_keymap::Keymap;
use spur_rpc::{Caps, ClientKind, Node, ServerMsg, Size, SurfaceView};

use crate::ux::state::UiState;

/// Frames a client may have in flight without acking.
pub const MAX_UNACKED: u64 = 2;
/// Most surface rows drawn for one client, whatever size it claims.
pub const MAX_ROWS: u32 = 500;
/// Pings kept waiting for a frame; more are dropped (a client that never acks).
const MAX_PINGS: usize = 1024;

/// Shuts a connection down once, from whichever side gets there first.
#[derive(Clone)]
pub struct Shutter(Arc<Mutex<Option<Close>>>);

type Close = Box<dyn FnOnce() + Send>;

impl Shutter {
    pub fn new(close: impl FnOnce() + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some(Box::new(close)))))
    }

    pub fn close(&self) {
        let f = self.0.lock().ok().and_then(|mut f| f.take());
        if let Some(f) = f {
            f();
        }
    }
}

/// One attached client.
pub struct Client {
    pub id: ClientId,
    pub kind: ClientKind,
    pub name: String,
    pub caps: Caps,
    pub size: Size,
    /// Rows and columns the client's layout gave the surface, once it said so.
    pub surface: Option<(u32, u32)>,
    pub active: ViewId,
    pub actor: Actor,
    pub keymap: Keymap,
    /// Client context keys, layered over the core's global context.
    pub scope: Context,
    pub sub: Option<Subscription>,
    pub last_ui: Option<Node>,
    /// UI state, for a frontend of a session with a pack.
    pub ui: Option<UiState>,
    /// The last surface sent, in full: the base the client builds the next one on.
    pub last_view: Option<SurfaceView>,
    /// Number of the last surface sent.
    pub surface_seq: u64,
    /// Last frame number sent, and last acked.
    sent: u64,
    acked: u64,
    /// Something it shows may have changed.
    pub dirty: bool,
    /// Pings to answer once its frames are sent.
    pings: Vec<u64>,
    /// Messages for the batch sent at the end of this loop iteration.
    pub batch: Vec<ServerMsg>,
    out: SyncSender<Vec<ServerMsg>>,
    /// Shuts a stream connection down when the client is dropped as stalled.
    pub shutter: Option<Shutter>,
}

/// Why sending to a client failed.
pub enum SendFail {
    /// Its outbox is full: it stopped reading.
    Full,
    /// Its connection is gone.
    Gone,
}

impl Client {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ClientId,
        kind: ClientKind,
        name: String,
        caps: Caps,
        size: Size,
        active: ViewId,
        keymap: Keymap,
        out: SyncSender<Vec<ServerMsg>>,
    ) -> Self {
        let actor = if kind == ClientKind::Frontend {
            Actor::User
        } else {
            // A script is its own principal, so its edits never land in the human's
            // undo history. The id keeps two tools with one name apart.
            Actor::Remote {
                client: format!("{name}#{}", id.0).into(),
            }
        };
        Self {
            id,
            kind,
            name,
            caps,
            size,
            surface: None,
            active,
            actor,
            keymap,
            scope: Context::new(),
            sub: None,
            last_ui: None,
            ui: None,
            last_view: None,
            surface_seq: 0,
            sent: 0,
            acked: 0,
            dirty: kind == ClientKind::Frontend,
            pings: Vec::new(),
            batch: Vec::new(),
            out,
            shutter: None,
        }
    }

    /// Draws UI (gets trees and frames).
    pub fn draws(&self) -> bool {
        self.kind == ClientKind::Frontend
    }

    /// May be sent another frame.
    pub fn can_send(&self) -> bool {
        self.sent - self.acked < MAX_UNACKED
    }

    /// Records a cumulative ack; numbers never sent are capped.
    pub fn on_ack(&mut self, frame: u64) {
        self.acked = self.acked.max(frame.min(self.sent));
    }

    /// Numbers the next frame.
    pub fn next_frame(&mut self) -> u64 {
        self.sent += 1;
        self.sent
    }

    /// Queues a ping for an answer after the frames it waits for.
    pub fn ping(&mut self, id: u64) {
        if self.pings.len() < MAX_PINGS {
            self.pings.push(id);
        }
    }

    /// Answers every queued ping. False if there were none.
    pub fn pong(&mut self) -> bool {
        if self.pings.is_empty() {
            return false;
        }
        let frame = self.sent;
        for id in self.pings.drain(..) {
            self.batch.push(ServerMsg::Pong { id, frame });
        }
        true
    }

    /// Text rows of the surface (at most [`MAX_ROWS`]). A pixel-layout client has none
    /// until its `layout` message says how many fit.
    pub fn rows(&self) -> usize {
        let rows = match self.surface {
            Some((rows, _)) => rows,
            None if self.caps.layout == spur_rpc::Layout::Pixels => 0,
            None => self.size.rows,
        };
        rows.min(MAX_ROWS) as usize
    }

    /// Rows a list is windowed to before the client reports its size.
    pub fn rows_hint(&self) -> u32 {
        self.size.rows.min(MAX_ROWS)
    }

    /// Refreshes the client's context keys from its active view.
    pub fn refresh_scope(&mut self, ed: &Editor) {
        let kind = if self.draws() { "frontend" } else { "tool" };
        // Keys are valid names, so `set` cannot fail.
        let _ = self.scope.set("client.kind", kind);
        let _ = self.scope.set("client.name", self.name.as_str());
        if let Some(keys) = ed.view_keys(self.active) {
            for (key, value) in keys {
                let _ = self.scope.set(key, value);
            }
        }
    }

    /// Sends this iteration's batch, if any.
    pub fn send_batch(&mut self) -> Result<(), SendFail> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.batch);
        self.out.try_send(batch).map_err(|e| match e {
            TrySendError::Full(_) => SendFail::Full,
            TrySendError::Disconnected(_) => SendFail::Gone,
        })
    }
}
