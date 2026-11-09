//! Keymap and mode engine. Bindings come in layers (core default, presets, user config,
//! extensions); later layers win. Presets are data (TOML layers), executed here.
//!
//! The engine maps keys to [`Dispatch`]es (command steps plus count, register and
//! captured chars); the caller runs them through `spur_core::Core::execute`.

mod key;
mod keymap;

pub use key::{Key, KeyCode, KeyParseError, Mods, format_keys, parse_keys};
pub use keymap::{
    Action, Binding, BindingSpec, Conflict, ConflictKind, DEFAULT_TIMEOUT, Described, Dispatch,
    Feed, Group, GroupSpec, KeyDescription, Keymap, Layer, LayerError, LayerSpec, MenuItem,
    ModeOptions, Status, Step, StepSpec,
};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-keymap");
    }
}
