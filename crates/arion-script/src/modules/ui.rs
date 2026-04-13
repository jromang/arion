//! Scriptable GUI: declarative widgets driven from Rhai.
//!
//! Builder semantics: `window(id, title, body)` pushes a fresh frame
//! onto `UiState::build_stack`, evaluates `body` (a `FnPtr`) via
//! `call_within_context` so its nested `label`/`button`/... calls fall
//! back into this module and push into the current top frame, then pops
//! the frame and wraps it into a `Widget::VBox` as the window root.
//!
//! Deferred callbacks (button clicks, `on_change`, `menu_item`) are
//! stored in `UiState::callbacks` by a generated id; the egui renderer
//! (phase 4) enqueues the corresponding `FnHandle` and the engine
//! dispatches it after the UI frame.

use rhai::{Dynamic, Engine, FnPtr, NativeCallContext};

use crate::ctx::ApiCtx;
use crate::modules::{rhai_err, ScriptModule};
use crate::ui_tree::{FnHandle, ScriptWindow, Widget};

pub struct UiModule;

impl ScriptModule for UiModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx) {
        // --- container: window(id, title, body) -----------------------
        let c = ctx.clone();
        engine.register_fn(
            "window",
            move |nctx: NativeCallContext, id: &str, title: &str, body: FnPtr|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                c.ui_state.borrow_mut().build_stack.push(Vec::new());
                let call_res = body.call_within_context::<Dynamic>(&nctx, ());
                let frame = c.ui_state.borrow_mut().build_stack.pop().unwrap_or_default();
                let _ = call_res?;

                let root = Widget::VBox(frame);
                let mut ui = c.ui_state.borrow_mut();
                let entry = ui.windows.entry(id.to_string()).or_insert_with(|| ScriptWindow {
                    id:    id.to_string(),
                    title: title.to_string(),
                    open:  true,
                    root:  Widget::VBox(Vec::new()),
                });
                entry.title = title.to_string();
                entry.root  = root;
                Ok(())
            },
        );

        // --- containers: vbox / hbox ----------------------------------
        register_box(engine, ctx, "vbox", true);
        register_box(engine, ctx, "hbox", false);

        // --- leaf widgets ---------------------------------------------
        let c = ctx.clone();
        engine.register_fn("label", move |text: &str| {
            push_widget(&c, Widget::Label(text.to_string()));
        });

        let c = ctx.clone();
        engine.register_fn(
            "button",
            move |label: &str, on_click: FnPtr| {
                let id = c.ui_state.borrow_mut().alloc_callback(on_click);
                push_widget(&c, Widget::Button {
                    label:    label.to_string(),
                    on_click: FnHandle::new(id),
                });
            },
        );

        let c = ctx.clone();
        engine.register_fn(
            "slider",
            move |label: &str, key: &str, min: f64, max: f64| {
                let mut ui = c.ui_state.borrow_mut();
                ui.values.entry(key.to_string()).or_insert_with(|| Dynamic::from(min));
                drop(ui);
                push_widget(&c, Widget::Slider {
                    label:     label.to_string(),
                    state_key: key.to_string(),
                    min, max,
                });
            },
        );

        let c = ctx.clone();
        engine.register_fn("checkbox", move |label: &str, key: &str| {
            let mut ui = c.ui_state.borrow_mut();
            ui.values.entry(key.to_string()).or_insert_with(|| Dynamic::from(false));
            drop(ui);
            push_widget(&c, Widget::Checkbox {
                label:     label.to_string(),
                state_key: key.to_string(),
            });
        });

        let c = ctx.clone();
        engine.register_fn("text_edit", move |key: &str| {
            let mut ui = c.ui_state.borrow_mut();
            ui.values.entry(key.to_string()).or_insert_with(|| Dynamic::from(String::new()));
            drop(ui);
            push_widget(&c, Widget::TextEdit { state_key: key.to_string() });
        });

        let c = ctx.clone();
        engine.register_fn("separator", move || {
            push_widget(&c, Widget::Separator);
        });

        // --- on_change(key, fn) ---------------------------------------
        let c = ctx.clone();
        engine.register_fn("on_change", move |key: &str, f: FnPtr| {
            let id = c.ui_state.borrow_mut().alloc_callback(f);
            c.ui_state.borrow_mut()
                .on_change.insert(key.to_string(), FnHandle::new(id));
        });

        // --- menu_item(path, fn) --------------------------------------
        let c = ctx.clone();
        engine.register_fn("menu_item", move |path: &str, f: FnPtr| {
            let id = c.ui_state.borrow_mut().alloc_callback(f);
            c.ui_state.borrow_mut()
                .menu_items.push((path.to_string(), FnHandle::new(id)));
        });

        // --- window_show / window_hide / window_toggle ----------------
        let c = ctx.clone();
        engine.register_fn("window_show", move |id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let mut ui = c.ui_state.borrow_mut();
            ui.windows.get_mut(id)
                .ok_or_else(|| rhai_err(format!("no such window: {id}")))?
                .open = true;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("window_hide", move |id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let mut ui = c.ui_state.borrow_mut();
            ui.windows.get_mut(id)
                .ok_or_else(|| rhai_err(format!("no such window: {id}")))?
                .open = false;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("window_toggle", move |id: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let mut ui = c.ui_state.borrow_mut();
            let w = ui.windows.get_mut(id)
                .ok_or_else(|| rhai_err(format!("no such window: {id}")))?;
            w.open = !w.open;
            Ok(())
        });
    }
}

fn register_box(engine: &mut Engine, ctx: &ApiCtx, name: &'static str, vertical: bool) {
    let c = ctx.clone();
    engine.register_fn(
        name,
        move |nctx: NativeCallContext, body: FnPtr| -> Result<(), Box<rhai::EvalAltResult>> {
            c.ui_state.borrow_mut().build_stack.push(Vec::new());
            let call_res = body.call_within_context::<Dynamic>(&nctx, ());
            let frame = c.ui_state.borrow_mut().build_stack.pop().unwrap_or_default();
            let _ = call_res?;
            let widget = if vertical { Widget::VBox(frame) } else { Widget::HBox(frame) };
            push_widget(&c, widget);
            Ok(())
        },
    );
}

fn push_widget(ctx: &ApiCtx, w: Widget) {
    let mut ui = ctx.ui_state.borrow_mut();
    if let Some(top) = ui.build_stack.last_mut() {
        top.push(w);
    }
    // Outside a builder: silently drop. Leaf calls only matter inside
    // a window/vbox/hbox body; this keeps REPL lines like `label("x")`
    // from panicking at top-level.
}
