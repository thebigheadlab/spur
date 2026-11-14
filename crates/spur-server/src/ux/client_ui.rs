//! A client's UI glue that needs its keymap, scope and view: key modes, key hints,
//! the palette's rows, and opening and closing the palette.

use std::sync::Arc;

use spur_core::editor::Editor;
use spur_core::{Context, ContextLookup, Scoped};

use super::palette::{ActionRow, Input, Matcher, Notify, PaletteState, Rows};
use super::state::{focused_input, shown};
use super::{EDITOR_REGION, Ux};
use crate::client::Client;

/// The key mode of `region`: its own, or the view's for the editor.
pub(crate) fn mode_of(ux: &Ux, c: &Client, ed: &Editor, region: &str) -> Arc<str> {
    ux.pack
        .regions
        .get(region)
        .and_then(|r| r.mode.as_deref())
        .map_or_else(|| view_mode(c, ed), Arc::from)
}

fn view_mode(c: &Client, ed: &Editor) -> Arc<str> {
    ed.views
        .get(&c.active)
        .map_or_else(|| ed.default_mode.clone(), |v| v.mode.clone())
}

/// The mode keys are read in now: the focused input's, else the focused region's,
/// else the view's.
pub(crate) fn key_mode(ux: Option<&Ux>, c: &Client, ed: &Editor, global: &Context) -> Arc<str> {
    let (Some(ux), Some(ui)) = (ux, c.ui.as_ref()) else {
        return view_mode(c, ed);
    };
    let look = Scoped(&[global, &c.scope]);
    if let Some(m) = focused_input(&ux.pack, ui, &look)
        .and_then(|i| ux.pack.nodes.get(&i))
        .and_then(|n| n.mode.as_deref())
    {
        return m.into();
    }
    mode_of(ux, c, ed, &ui.focus)
}

/// The hint for action `action` shown in `node`: resolved in the mode of the
/// node's region, or of the focused region for nodes outside regions and in
/// overlays.
pub(crate) fn hint(
    ux: &Ux,
    c: &Client,
    ed: &Editor,
    look: &impl ContextLookup,
    action: &str,
    node: &str,
) -> Option<String> {
    let a = ux.pack.action(action)?;
    let focus = c.ui.as_ref().map_or(EDITOR_REGION, |u| u.focus.as_str());
    let region = if ux.pack.overlay_of(node).is_some() {
        focus
    } else {
        ux.pack.region_of(node).unwrap_or(focus)
    };
    let mode = mode_of(ux, c, ed, region);
    a.hint(
        &c.keymap,
        &ux.pack.keys.name,
        &mode,
        look,
        &ux.pack.key_display,
    )
}

/// The palette's query.
pub(crate) fn query(ux: &Ux, c: &Client) -> String {
    let (Some(p), Some(ui)) = (ux.pack.palette.as_ref(), c.ui.as_ref()) else {
        return String::new();
    };
    ui.fields
        .get(&p.input)
        .map(|f| f.text.clone())
        .unwrap_or_default()
}

/// True if the palette is open for the client.
pub(crate) fn palette_open(ux: &Ux, c: &Client) -> bool {
    match (ux.pack.palette.as_ref(), c.ui.as_ref()) {
        (Some(p), Some(ui)) => ui.overlay.as_deref() == Some(p.overlay.as_str()),
        _ => false,
    }
}

/// The palette's rows now, if it is open.
pub(crate) fn palette_rows(ux: &Ux, c: &Client, ed: &Editor, global: &Context) -> Option<Rows> {
    if !palette_open(ux, c) {
        return None;
    }
    let ui = c.ui.as_ref()?;
    let q = query(ux, c);
    // Actions are listed as the remembered region would run them.
    let region = ui.prev_focus.as_deref().unwrap_or(EDITOR_REGION);
    let mode = mode_of(ux, c, ed, region);
    let mut over = Context::new();
    let _ = over.set("ui.focus", region);
    let look = Scoped(&[global, &c.scope, &over]);
    let actions = if matches!(ui.palette.scope.as_str(), "all" | "actions") {
        ux.pack
            .actions
            .iter()
            .filter(|(_, a)| a.title.is_some() && a.modes.iter().any(|m| **m == *mode))
            .filter(|(_, a)| a.when.as_ref().is_none_or(|w| w.eval(&look)))
            .map(|(id, a)| ActionRow {
                id: id.clone(),
                title: a.title.clone().unwrap_or_default(),
                key: a.hint(
                    &c.keymap,
                    &ux.pack.keys.name,
                    &mode,
                    &look,
                    &ux.pack.key_display,
                ),
            })
            .collect()
    } else {
        Vec::new()
    };
    let current = ed.views.get(&c.active).and_then(|v| ed.docs.get(&v.doc));
    let current_path = current.and_then(|d| d.path.as_deref());
    let recent = ui
        .recent
        .iter()
        .filter(|p| Some(p.as_path()) != current_path)
        .cloned()
        .collect();
    let lines = current.map_or(1, |d| d.buffer.snapshot().line_count());
    let input = Input {
        pack: &ux.pack,
        query: &q,
        scope: &ui.palette.scope,
        actions,
        recent,
        root: ux.index.root(),
        status: ux.index.status(),
        lines,
    };
    Some(Rows::build(&input, ux.matchers.get(&c.id.0)))
}

/// Opens the palette in `scope`, or switches scope (keeping the query) if open.
///
/// # Errors
///
/// The pack has no palette.
pub(crate) fn open_palette(
    ux: &mut Ux,
    c: &mut Client,
    scope: &str,
    query: Option<&str>,
) -> Result<(), String> {
    let Some(spec) = ux.pack.palette.clone() else {
        return Err("this pack has no palette".into());
    };
    let conn = c.id.0;
    let open = palette_open(ux, c);
    let Some(ui) = c.ui.as_mut() else {
        return Err("this client has no UI state".into());
    };
    if open {
        set_scope(&mut ui.palette, scope);
    } else {
        ui.prev_focus = Some(std::mem::replace(&mut ui.focus, spec.region.clone()));
        ui.overlay = Some(spec.overlay.clone());
        ui.input = Some(spec.input.clone());
        let generation = ui.palette.generation + 1;
        ui.palette = PaletteState {
            scope: scope.to_owned(),
            open_scope: scope.to_owned(),
            follow: true,
            generation,
            ..PaletteState::default()
        };
        let field = ui.fields.entry(spec.input.clone()).or_default();
        field.clear();
        if let Some(q) = query {
            field.insert(q);
        }
        c.keymap.reset();
        // The index is walked when a palette first opens, and again when stale.
        let status = ux.index.status();
        if ux.index.stale() && (status.done || status.generation == 0) {
            let notify = ux.notify.clone();
            let generation = ux.index.walk(notify);
            for (k, m) in &mut ux.matchers {
                if m.index_generation != generation {
                    m.refill(*k, &ux.index);
                }
            }
        }
        if !ux.matchers.contains_key(&conn) {
            let notify = ux
                .notify
                .clone()
                .unwrap_or_else(|| Notify(Arc::new(|_| {})));
            let m = Matcher::new(conn, &notify, &ux.index);
            ux.matchers.insert(conn, m);
        }
    }
    c.dirty = true;
    query_changed(ux, c);
    Ok(())
}

/// Switches scope: a new generation, the selection back on the first row.
pub(crate) fn set_scope(st: &mut PaletteState, scope: &str) {
    if st.scope != scope {
        scope.clone_into(&mut st.scope);
        st.generation += 1;
        st.follow = true;
        st.offset = 0;
    }
}

/// Closes the palette; focus goes back where it was (or to the editor if that
/// region is no longer shown).
pub(crate) fn close_palette(ux: &Ux, c: &mut Client, global: &Context) {
    if !palette_open(ux, c) {
        return;
    }
    let Some(ui) = c.ui.as_mut() else {
        return;
    };
    ui.overlay = None;
    ui.input = None;
    let back = ui.prev_focus.take().unwrap_or_else(|| EDITOR_REGION.into());
    let ok = ux
        .pack
        .regions
        .get(&back)
        .is_some_and(|r| shown(&ux.pack, &r.node, &Scoped(&[global, &c.scope])));
    ui.focus = if ok { back } else { EDITOR_REGION.into() };
    c.keymap.reset();
    c.dirty = true;
}

/// The query changed: a new generation, the selection back on the first row, and
/// the file matcher told (it matches off this thread).
pub(crate) fn query_changed(ux: &mut Ux, c: &mut Client) {
    let q = query(ux, c);
    let Some(ui) = c.ui.as_mut() else {
        return;
    };
    ui.palette.generation += 1;
    ui.palette.follow = true;
    ui.palette.offset = 0;
    if matches!(ui.palette.scope.as_str(), "all" | "files")
        && let Some(m) = ux.matchers.get_mut(&c.id.0)
    {
        m.set_query(q.trim());
    }
}
