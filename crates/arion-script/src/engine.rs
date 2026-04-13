//! The [`ScriptEngine`] facade. Owns a `rhai::Engine`, a `Scope`, a
//! REPL output buffer, and the [`ApiCtx`] shared with every module.

use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine, Scope};

use arion_app::App;

use crate::ctx::{ApiCtx, UiStateRc};
use crate::error::ScriptError;
use crate::modules::{radio::Radio, register_builtins};
use crate::ui_tree::FnHandle;

#[derive(Debug, Clone)]
pub struct ReplLine {
    pub kind: ReplLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplLineKind {
    Input,
    Result,
    Error,
    Print,
}

pub struct ScriptEngine {
    engine:  Engine,
    scope:   Scope<'static>,
    ctx:     ApiCtx,
    output:  Vec<ReplLine>,
    history: Vec<String>,
}

impl ScriptEngine {
    pub fn new() -> Self {
        let mut engine = Engine::new();
        engine.set_max_operations(1_000_000);

        let ctx = ApiCtx::new();
        register_builtins(&mut engine, &ctx);

        let mut scope = Scope::new();
        scope.push_constant("radio", Radio);

        ScriptEngine {
            engine,
            scope,
            ctx,
            output:  Vec::new(),
            history: Vec::new(),
        }
    }

    pub fn ctx(&self) -> &ApiCtx { &self.ctx }

    /// Shared handle to the live `UiState`. The egui renderer reads
    /// this once per frame to draw scripted windows.
    pub fn ui_state(&self) -> UiStateRc { self.ctx.ui_state.clone() }

    /// Dispatch a deferred UI callback (button click, on_change,
    /// menu item). Looks up the stored `FnPtr` by `handle.name` in
    /// `UiState::callbacks`, binds `app`, and calls it with
    /// `handle.args`.
    pub fn dispatch_callback(&mut self, handle: &FnHandle, app: &mut App) -> Result<(), ScriptError> {
        let fn_ptr_opt = self.ctx.ui_state.borrow().callbacks.get(&handle.name).cloned();
        let Some(fn_ptr) = fn_ptr_opt else {
            return Err(ScriptError::InvalidArgument(format!("unknown callback id: {}", handle.name)));
        };
        self.bind_app(app);
        let ast = rhai::AST::empty();
        let res = fn_ptr.call::<Dynamic>(&self.engine, &ast, handle.args.clone());
        self.unbind_app();
        res.map(|_| ()).map_err(ScriptError::from)
    }

    /// Execute a multi-statement Rhai script against `app`. Unlike
    /// [`run_line`], the raw source is not echoed to the REPL (script
    /// files are generally large and the user doesn't want to see them
    /// replayed). Errors surface as a single `Error` line so the user
    /// still gets feedback from the REPL.
    pub fn run_script(&mut self, source: &str, app: &mut App) -> Result<(), ScriptError> {
        let print_buf: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let pb = print_buf.clone();
        self.engine.on_print(move |s| {
            if let Ok(mut buf) = pb.lock() {
                buf.push(s.to_string());
            }
        });

        self.bind_app(app);
        let result = self.engine.eval_with_scope::<Dynamic>(&mut self.scope, source);
        self.unbind_app();

        let printed: Vec<String> = print_buf.lock().map(|b| b.clone()).unwrap_or_default();
        for line in printed {
            self.output.push(ReplLine { kind: ReplLineKind::Print, text: line });
        }

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = format!("{e}");
                self.output.push(ReplLine { kind: ReplLineKind::Error, text: msg.clone() });
                Err(ScriptError::Rhai(msg))
            }
        }
    }

    /// Push a synthetic line into the REPL output buffer. Used by the
    /// frontend to surface e.g. startup-script load errors.
    pub fn push_output(&mut self, kind: ReplLineKind, text: impl Into<String>) {
        self.output.push(ReplLine { kind, text: text.into() });
    }

    /// Execute one line of Rhai code against `app`. The pointer to
    /// `app` is bound only for the duration of this call.
    pub fn run_line(&mut self, line: &str, app: &mut App) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        self.history.push(trimmed.to_string());
        self.output.push(ReplLine {
            kind: ReplLineKind::Input,
            text: format!("> {trimmed}"),
        });

        let print_buf: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let pb = print_buf.clone();
        self.engine.on_print(move |s| {
            if let Ok(mut buf) = pb.lock() {
                buf.push(s.to_string());
            }
        });

        self.bind_app(app);
        let result = self.engine.eval_with_scope::<Dynamic>(&mut self.scope, trimmed);
        self.unbind_app();

        match result {
            Ok(val) => {
                let text = if val.is_unit() { "ok".to_string() } else { format!("{val}") };
                self.output.push(ReplLine { kind: ReplLineKind::Result, text });
            }
            Err(e) => {
                self.output.push(ReplLine { kind: ReplLineKind::Error, text: format!("{e}") });
            }
        }

        let printed: Vec<String> = print_buf.lock().map(|b| b.clone()).unwrap_or_default();
        for line in printed {
            self.output.push(ReplLine { kind: ReplLineKind::Print, text: line });
        }
    }

    /// Invoke a deferred [`FnHandle`] callback with `app` bound.
    /// Used by the egui renderer after a button click (phase 4+).
    pub fn invoke_callback(&mut self, handle: &FnHandle, app: &mut App) -> Result<(), ScriptError> {
        self.bind_app(app);
        let res = self.engine
            .call_fn::<Dynamic>(&mut self.scope, &rhai::AST::empty(), &handle.name, handle.args.clone());
        self.unbind_app();
        res.map(|_| ()).map_err(ScriptError::from)
    }

    pub fn output(&self) -> &[ReplLine] { &self.output }
    pub fn history(&self) -> &[String] { &self.history }
    pub fn clear_output(&mut self) { self.output.clear(); }

    fn bind_app(&self, app: &mut App) {
        *self.ctx.app.borrow_mut() = Some(app as *mut App);
    }
    fn unbind_app(&self) {
        *self.ctx.app.borrow_mut() = None;
    }
}

impl Default for ScriptEngine {
    fn default() -> Self { Self::new() }
}
