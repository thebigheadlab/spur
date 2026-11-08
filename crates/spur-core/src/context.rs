//! Context keys: named values that `when` clauses test.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::name::{NameError, check_name};

/// A context key value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CtxValue {
    /// A flag.
    Bool(bool),
    /// A number.
    Int(i64),
    /// A string.
    Str(Arc<str>),
    /// A list of strings, for `in`.
    List(Arc<[Arc<str>]>),
}

impl CtxValue {
    /// Truthiness used by a bare key in a `when` clause.
    pub fn truthy(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Int(i) => *i != 0,
            Self::Str(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
        }
    }
}

impl From<bool> for CtxValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<i64> for CtxValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl From<&str> for CtxValue {
    fn from(v: &str) -> Self {
        Self::Str(v.into())
    }
}

impl From<String> for CtxValue {
    fn from(v: String) -> Self {
        Self::Str(v.into())
    }
}

impl From<Vec<&str>> for CtxValue {
    fn from(v: Vec<&str>) -> Self {
        Self::List(v.into_iter().map(Arc::from).collect())
    }
}

/// Anything `when` clauses can read keys from.
pub trait ContextLookup {
    /// The value of `key`, if set.
    fn get(&self, key: &str) -> Option<&CtxValue>;
}

/// One scope of context keys (global, client, view, widget, ...).
#[derive(Clone, Debug, Default)]
pub struct Context {
    keys: HashMap<Arc<str>, CtxValue>,
    generation: u64,
}

impl Context {
    /// An empty scope.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `key`, returning the old value. Keys use the dotted name grammar.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if `key` is not a dotted name.
    pub fn set(
        &mut self,
        key: &str,
        value: impl Into<CtxValue>,
    ) -> Result<Option<CtxValue>, NameError> {
        let value = value.into();
        if let Some(slot) = self.keys.get_mut(key) {
            if *slot == value {
                return Ok(Some(value));
            }
            self.generation += 1;
            return Ok(Some(std::mem::replace(slot, value)));
        }
        check_name(key, 2)?;
        self.generation += 1;
        self.keys.insert(key.into(), value);
        Ok(None)
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &str) -> Option<CtxValue> {
        let old = self.keys.remove(key);
        if old.is_some() {
            self.generation += 1;
        }
        old
    }

    /// Increases on every change, so callers (key hints, menus) can cache results.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// All keys and values, sorted by key (for `inspect_context`).
    pub fn entries(&self) -> Vec<(&str, &CtxValue)> {
        let mut v: Vec<_> = self.keys.iter().map(|(k, v)| (&**k, v)).collect();
        v.sort_unstable_by_key(|(k, _)| *k);
        v
    }
}

impl ContextLookup for Context {
    fn get(&self, key: &str) -> Option<&CtxValue> {
        self.keys.get(key)
    }
}

/// Layered lookup: later scopes override earlier ones (global < client < view).
#[derive(Clone, Copy, Debug)]
pub struct Scoped<'a>(pub &'a [&'a Context]);

impl ContextLookup for Scoped<'_> {
    fn get(&self, key: &str) -> Option<&CtxValue> {
        self.0.iter().rev().find_map(|c| c.get(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_generation() {
        let mut c = Context::new();
        assert!(c.set("Bad", true).is_err());
        assert_eq!(c.generation(), 0);
        c.set("editor.mode", "normal").unwrap();
        let g = c.generation();
        c.set("editor.mode", "normal").unwrap();
        assert_eq!(c.generation(), g, "same value is not a change");
        assert_eq!(
            c.set("editor.mode", "insert").unwrap(),
            Some("normal".into())
        );
        assert!(c.generation() > g);
        assert_eq!(c.get("editor.mode"), Some(&"insert".into()));
        assert_eq!(c.remove("editor.mode"), Some("insert".into()));
        assert!(c.get("editor.mode").is_none());
    }

    #[test]
    fn scoped_later_wins() {
        let mut global = Context::new();
        global.set("editor.mode", "normal").unwrap();
        global.set("ui.theme", "dark").unwrap();
        let mut view = Context::new();
        view.set("editor.mode", "insert").unwrap();
        let scopes = [&global, &view];
        let s = Scoped(&scopes);
        assert_eq!(s.get("editor.mode"), Some(&"insert".into()));
        assert_eq!(s.get("ui.theme"), Some(&"dark".into()));
    }
}
