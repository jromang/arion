//! Shared context passed to every script module at registration time.
//!
//! # Why the raw pointer + RefCell dance
//!
//! Rhai's registered functions are `'static` closures. They therefore
//! cannot capture a `&mut App` with a scope lifetime. At the same time,
//! we refuse to give the scripting layer long-lived ownership of the
//! `App` — the single-source-of-truth invariant says only the frontend
//! owns it (`&mut App`). So we thread the borrow through a tiny FFI-
//! style shim:
//!
//! - [`AppHandle`] is an `Rc<RefCell<Option<*mut App>>>`.
//! - `ScriptEngine::run_line` writes the pointer at entry, clears it
//!   at exit. The pointer is valid exactly for the duration of the
//!   call, never longer.
//! - Closures captured in `Rc` form call [`AppHandle::with_app`] which
//!   dereferences the pointer under a `RefCell` borrow — this gives
//!   us a checked `&mut App` with a scope that cannot escape the
//!   closure body.
//!
//! The `unsafe` is bounded to that one `as_mut()` call, and the fact
//! that we clear the slot before `run_line` returns means no stale
//! pointer can be observed on a later call.

use std::cell::RefCell;
use std::rc::Rc;

use arion_app::App;

use crate::error::ScriptError;
use crate::ui_tree::UiState;

pub type AppHandle  = Rc<RefCell<Option<*mut App>>>;
pub type UiStateRc  = Rc<RefCell<UiState>>;

/// Passed to every [`ScriptModule::register`](crate::modules::ScriptModule::register).
/// Cheaply cloneable — all inner handles are `Rc`-wrapped.
#[derive(Clone)]
pub struct ApiCtx {
    pub app:      AppHandle,
    pub ui_state: UiStateRc,
}

impl ApiCtx {
    pub fn new() -> Self {
        ApiCtx {
            app:      Rc::new(RefCell::new(None)),
            ui_state: Rc::new(RefCell::new(UiState::default())),
        }
    }

    /// Run `f` with a scoped `&mut App`, or return `AppUnbound` if
    /// no app is currently bound.
    ///
    /// SAFETY: the raw pointer stored in `self.app` is only set by
    /// `ScriptEngine::bind_app` for the duration of a single
    /// `run_line` / `invoke_callback` call and cleared before that
    /// call returns. The `&mut App` reference produced here therefore
    /// cannot outlive the original borrow held by the frontend.
    pub fn with_app<R>(&self, f: impl FnOnce(&mut App) -> R) -> Result<R, ScriptError> {
        let slot = self.app.borrow();
        let ptr = slot.ok_or(ScriptError::AppUnbound)?;
        // SAFETY: see function doc.
        let app = unsafe { &mut *ptr };
        Ok(f(app))
    }
}

impl Default for ApiCtx {
    fn default() -> Self {
        Self::new()
    }
}
