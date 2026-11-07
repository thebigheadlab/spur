//! Native desktop frontend (winit + wgpu). A client of the session server.
//!
//! Stub crate. See the architecture spec: <https://github.com/thebigheadlab/spur/issues/3>.

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-gui");
    }
}
