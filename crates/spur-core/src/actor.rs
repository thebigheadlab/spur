//! Who acted (the principal), separate from how they connected (the client).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use spur_text::{AuthorId, GroupKey};

/// The principal behind a command or event. A human is [`Actor::User`] from any
/// frontend, so undo keeps working after reattaching with a new [`ClientId`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// A human, from any frontend.
    User,
    /// An agent turn.
    Agent {
        /// Agent session; becomes the undo author.
        session_id: Arc<str>,
        /// Kind of agent, e.g. an ACP agent name.
        agent_kind: Arc<str>,
        /// Turn within the session; becomes the undo group.
        turn_id: u64,
        /// Tool call that issued the command, if any.
        tool_call_id: Option<Arc<str>>,
    },
    /// An extension, by id.
    Extension {
        /// Extension id (one segment).
        id: Arc<str>,
    },
    /// A scripted RPC principal (not a human frontend).
    Remote {
        /// Principal name.
        client: Arc<str>,
    },
    /// The core itself: disk reloads, timers, the bus.
    System {
        /// What inside the core acted.
        source: Arc<str>,
    },
}

impl Actor {
    /// The text-engine author for edits made by this actor.
    pub fn author(&self) -> AuthorId {
        match self {
            Self::User => AuthorId::User,
            Self::Agent { session_id, .. } => AuthorId::Agent(session_id.clone()),
            Self::Extension { id } => AuthorId::Extension(id.clone()),
            Self::Remote { client } => AuthorId::Rpc(client.clone()),
            Self::System { source } => AuthorId::External(source.clone()),
        }
    }

    /// The agent turn, if this is an agent.
    pub fn turn_id(&self) -> Option<u64> {
        match self {
            Self::Agent { turn_id, .. } => Some(*turn_id),
            _ => None,
        }
    }

    /// Undo group: agents group by their turn, everyone else by the core-assigned `turn`.
    pub fn group_key(&self, turn: u64) -> GroupKey {
        GroupKey {
            actor: self.author(),
            turn: self.turn_id().unwrap_or(turn),
        }
    }
}

/// A frontend or RPC connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        let agent = Actor::Agent {
            session_id: "s1".into(),
            agent_kind: "acp".into(),
            turn_id: 7,
            tool_call_id: None,
        };
        assert_eq!(agent.author(), AuthorId::Agent("s1".into()));
        assert_eq!(agent.group_key(99).turn, 7);
        assert_eq!(Actor::User.group_key(3).turn, 3);
        assert_eq!(
            Actor::System {
                source: "disk".into()
            }
            .author(),
            AuthorId::External("disk".into())
        );
    }

    #[test]
    fn serde_shape() {
        let json = serde_json::to_value(Actor::Extension { id: "git".into() }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "extension", "id": "git"}));
    }
}
