//! UI tree value objects: descriptors used by scripted windows.
//!
//! These types are pure data — no egui, no rendering. Phase 3 wires
//! builder-style Rhai functions that populate a [`UiState`]; phase 4
//! adds an egui renderer that walks the tree. For phase 2 they exist
//! only because [`crate::ctx::ApiCtx`] holds a handle to a `UiState`.

use std::collections::HashMap;

use rhai::{Dynamic, FnPtr};

#[derive(Clone, Debug)]
pub enum Widget {
    Label(String),
    Button {
        label:    String,
        on_click: FnHandle,
    },
    Checkbox {
        label:     String,
        state_key: String,
    },
    Slider {
        label:     String,
        state_key: String,
        min:       f64,
        max:       f64,
    },
    TextEdit {
        state_key: String,
    },
    Separator,
    HBox(Vec<Widget>),
    VBox(Vec<Widget>),
}

#[derive(Clone, Debug)]
pub struct ScriptWindow {
    pub id:    String,
    pub title: String,
    pub open:  bool,
    pub root:  Widget,
}

/// A reference to a Rhai function to be invoked later (e.g. on button
/// click). Captured by name + args so it can be re-dispatched against
/// the engine AST after the UI frame is done.
#[derive(Clone, Debug)]
pub struct FnHandle {
    pub name: String,
    pub args: Vec<Dynamic>,
}

impl FnHandle {
    pub fn new(name: impl Into<String>) -> Self {
        FnHandle {
            name: name.into(),
            args: Vec::new(),
        }
    }
}

/// Live UI state shared between the script engine and the egui
/// renderer. `windows` holds the descriptor tree; `values` is the
/// backing store for sliders/checkboxes/textfields keyed by
/// `state_key`; `on_change` maps state_keys to callbacks.
#[derive(Default)]
pub struct UiState {
    pub windows:    HashMap<String, ScriptWindow>,
    pub menu_items: Vec<(String, FnHandle)>,
    pub values:     HashMap<String, Dynamic>,
    pub on_change:  HashMap<String, FnHandle>,
    /// Registry of stored FnPtrs keyed by generated id (referenced from
    /// [`FnHandle::name`]). Callbacks are dispatched later by looking up
    /// the pointer here and invoking it against the live engine+AST.
    pub callbacks:  HashMap<String, FnPtr>,
    /// Stack of in-progress child frames used by `window`/`vbox`/`hbox`
    /// while the builder body executes. Topmost frame collects widgets
    /// emitted by nested builder calls.
    pub build_stack: Vec<Vec<Widget>>,
    /// Monotonic counter for generating unique callback ids.
    pub next_cb_id: u64,
}

impl UiState {
    pub fn alloc_callback(&mut self, fn_ptr: FnPtr) -> String {
        let id = format!("__cb_{}", self.next_cb_id);
        self.next_cb_id += 1;
        self.callbacks.insert(id.clone(), fn_ptr);
        id
    }
}

impl std::fmt::Debug for UiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiState")
            .field("windows_count", &self.windows.len())
            .field("menu_items_count", &self.menu_items.len())
            .field("values_count", &self.values.len())
            .field("on_change_count", &self.on_change.len())
            .finish()
    }
}
