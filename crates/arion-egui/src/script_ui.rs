//! Renderer for scripted UI: walks [`arion_script::UiState`] and
//! translates [`Widget`] descriptors into egui calls.
//!
//! Called once per frame from [`crate::EguiView::ui`] after the native
//! windows have been drawn. Widget descriptors are pure data; this file
//! owns the egui-specific translation. Any value mutation (slider drag,
//! checkbox toggle, text edit) triggers the corresponding `on_change`
//! callback at end of frame after the `ui_state` borrow is released.

use eframe::egui;
use rhai::Dynamic;

use arion_app::App;
use arion_script::{FnHandle, ScriptEngine, Widget};

pub fn render_script_ui(
    ctx:           &egui::Context,
    script_engine: &mut ScriptEngine,
    app:           &mut App,
) {
    let state_rc = script_engine.ui_state();
    let mut pending: Vec<FnHandle> = Vec::new();

    {
        let mut ui_state = state_rc.borrow_mut();
        let window_ids: Vec<String> = ui_state.windows.keys().cloned().collect();

        for id in window_ids {
            let (title, mut open, root) = {
                let w = match ui_state.windows.get(&id) {
                    Some(w) => w,
                    None => continue,
                };
                (w.title.clone(), w.open, w.root.clone())
            };
            if !open {
                continue;
            }

            let mut clicks: Vec<FnHandle> = Vec::new();
            let mut changed_keys: Vec<String> = Vec::new();
            egui::Window::new(&title)
                .id(egui::Id::new(&id))
                .open(&mut open)
                .show(ctx, |ui| {
                    render_widget(ui, &root, &mut ui_state.values, &mut changed_keys, &mut clicks);
                });

            if let Some(w) = ui_state.windows.get_mut(&id) {
                w.open = open;
            }

            for h in clicks {
                pending.push(h);
            }
            for key in changed_keys {
                if let Some(h) = ui_state.on_change.get(&key).cloned() {
                    let arg = ui_state.values.get(&key).cloned().unwrap_or_default();
                    pending.push(FnHandle { name: h.name, args: vec![arg] });
                }
            }
        }
    }

    for h in pending {
        let _ = script_engine.dispatch_callback(&h, app);
    }
}

fn render_widget(
    ui:       &mut egui::Ui,
    widget:   &Widget,
    values:   &mut std::collections::HashMap<String, Dynamic>,
    changed:  &mut Vec<String>,
    clicks:   &mut Vec<FnHandle>,
) {
    match widget {
        Widget::Label(s) => { ui.label(s); }

        Widget::Button { label, on_click } => {
            if ui.button(label).clicked() {
                clicks.push(on_click.clone());
            }
        }

        Widget::Separator => { ui.separator(); }

        Widget::VBox(items) => {
            ui.vertical(|ui| {
                for it in items {
                    render_widget(ui, it, values, changed, clicks);
                }
            });
        }

        Widget::HBox(items) => {
            ui.horizontal(|ui| {
                for it in items {
                    render_widget(ui, it, values, changed, clicks);
                }
            });
        }

        Widget::Checkbox { label, state_key } => {
            let mut v = values.get(state_key)
                .and_then(|d| d.as_bool().ok())
                .unwrap_or(false);
            if ui.checkbox(&mut v, label).changed() {
                values.insert(state_key.clone(), Dynamic::from(v));
                changed.push(state_key.clone());
            }
        }

        Widget::Slider { label, state_key, min, max } => {
            let mut v = values.get(state_key)
                .and_then(|d| d.as_float().ok())
                .unwrap_or(*min);
            let resp = ui.add(egui::Slider::new(&mut v, *min..=*max).text(label));
            if resp.changed() {
                values.insert(state_key.clone(), Dynamic::from(v));
                changed.push(state_key.clone());
            }
        }

        Widget::TextEdit { state_key } => {
            let mut s = values.get(state_key)
                .and_then(|d| d.clone().into_string().ok())
                .unwrap_or_default();
            if ui.text_edit_singleline(&mut s).changed() {
                values.insert(state_key.clone(), Dynamic::from(s));
                changed.push(state_key.clone());
            }
        }
    }
}
