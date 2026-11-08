//! Command metadata and errors.

use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::name::CommandId;

/// MCP-style tool annotations, so commands can be exported as tools unchanged.
#[allow(clippy::struct_excessive_bools)] // mirrors the MCP annotation set
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotations {
    /// Does not change state.
    pub read_only: bool,
    /// May destroy data (delete files, discard changes).
    pub destructive: bool,
    /// Running twice with the same args equals running once.
    pub idempotent: bool,
    /// Touches the outside world (network, processes).
    pub open_world: bool,
}

/// Where a command should run when the frontend and the workspace are on different
/// machines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSide {
    /// Next to the user (clipboard, window, theme).
    Ui,
    /// Next to the files (edits, git, LSP).
    #[default]
    Workspace,
}

/// Who defined something: shown by `describe_command`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// Built into the core.
    Core,
    /// An extension, by id.
    Extension {
        /// Extension id.
        id: Arc<str>,
    },
    /// A config file.
    Config {
        /// File path.
        file: Arc<str>,
        /// 1-based line.
        line: u32,
    },
}

/// Static description of a command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandMeta {
    /// Unique id.
    pub id: CommandId,
    /// One-line description.
    pub description: String,
    /// JSON Schema for the args (`null` = no args).
    pub args_schema: Value,
    /// Tool annotations.
    pub annotations: Annotations,
    /// `.` may repeat it.
    pub repeatable: bool,
    /// Routing hint.
    pub remote_side: RemoteSide,
    /// Who registered it.
    pub source: Source,
}

impl CommandMeta {
    /// A core command with no args schema.
    pub fn new(id: CommandId, description: impl Into<String>) -> Self {
        Self {
            id,
            description: description.into(),
            args_schema: Value::Null,
            annotations: Annotations::default(),
            repeatable: false,
            remote_side: RemoteSide::Workspace,
            source: Source::Core,
        }
    }

    /// Sets the args schema.
    #[must_use]
    pub fn args_schema(mut self, schema: Value) -> Self {
        self.args_schema = schema;
        self
    }

    /// Sets the annotations.
    #[must_use]
    pub fn annotations(mut self, a: Annotations) -> Self {
        self.annotations = a;
        self
    }

    /// Marks it read-only.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.annotations.read_only = true;
        self
    }

    /// Marks it repeatable by `.`.
    #[must_use]
    pub fn repeatable(mut self) -> Self {
        self.repeatable = true;
        self
    }

    /// Sets the routing hint.
    #[must_use]
    pub fn remote_side(mut self, side: RemoteSide) -> Self {
        self.remote_side = side;
        self
    }

    /// Sets the source.
    #[must_use]
    pub fn source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }
}

/// Error category, shared with the RPC protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No such command.
    UnknownCommand,
    /// Args did not parse.
    BadArgs,
    /// The command ran and failed.
    Failed,
    /// A gate middleware refused it.
    Vetoed,
    /// The actor lacks a capability.
    PermissionDenied,
    /// Nested commands went deeper than allowed.
    TooDeep,
    /// A text edit conflicted with newer edits.
    Stale,
}

impl ErrorCode {
    /// The `snake_case` wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownCommand => "unknown_command",
            Self::BadArgs => "bad_args",
            Self::Failed => "failed",
            Self::Vetoed => "vetoed",
            Self::PermissionDenied => "permission_denied",
            Self::TooDeep => "too_deep",
            Self::Stale => "stale",
        }
    }
}

/// A command failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandError {
    /// Category.
    pub code: ErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Extra structured data.
    pub data: Option<Value>,
}

impl CommandError {
    /// An error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `failed`.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Failed, message)
    }

    /// `bad_args`.
    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadArgs, message)
    }

    /// `vetoed` by the named middleware.
    pub fn vetoed(by: &str, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Vetoed,
            message: message.into(),
            data: Some(serde_json::json!({ "by": by })),
        }
    }

    /// `unknown_command`.
    pub fn unknown(id: &str) -> Self {
        Self::new(ErrorCode::UnknownCommand, format!("unknown command {id}"))
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CommandError {}

/// Parses command args; `null` counts as `{}` so all-default structs accept no args.
///
/// # Errors
///
/// Returns `bad_args` if `args` do not deserialize into `T`.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, CommandError> {
    let r = if args.is_null() {
        serde_json::from_value(Value::Object(serde_json::Map::new()))
    } else {
        T::deserialize(args)
    };
    r.map_err(|e| CommandError::bad_args(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Args {
        #[serde(default)]
        n: u32,
    }

    #[test]
    fn args() {
        assert_eq!(parse_args::<Args>(&Value::Null).unwrap(), Args { n: 0 });
        assert_eq!(
            parse_args::<Args>(&serde_json::json!({"n": 2})).unwrap(),
            Args { n: 2 }
        );
        let e = parse_args::<Args>(&serde_json::json!({"n": "x"})).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadArgs);
    }

    #[test]
    fn codes_serialize_snake_case() {
        for c in [
            ErrorCode::UnknownCommand,
            ErrorCode::TooDeep,
            ErrorCode::PermissionDenied,
        ] {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }
}
