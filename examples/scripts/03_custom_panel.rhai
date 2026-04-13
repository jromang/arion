// 03_custom_panel.rhai — a custom panel with buttons and a slider.
//
// Declares a persistent window "My panel" containing:
// - the current frequency display;
// - +/- buttons to move the VFO;
// - a volume slider bound to RX 0;
// - an NR3 checkbox.
//
// The window stays visible until it is closed (egui's X button)
// or hidden via `window_hide("my-panel")`.

window("my-panel", "My panel", || {
    label("RX 0 — current frequency shown in the top bar.");

    hbox(|| {
        button("- 1 kHz",  || { radio[0].freq = radio.rx(0).freq - 1000; });
        button("+ 1 kHz",  || { radio[0].freq = radio.rx(0).freq + 1000; });
        button("- 100 Hz", || { radio[0].freq = radio.rx(0).freq -  100; });
        button("+ 100 Hz", || { radio[0].freq = radio.rx(0).freq +  100; });
    });

    separator();

    slider("Volume",   "vol", 0.0, 1.0);
    checkbox("NR3",    "nr3");
    checkbox("NB",     "nb");
    checkbox("Muted",  "muted");
});

// Bind the widgets to RX 0 state.
on_change("vol",   |v| { radio[0].volume = v; });
on_change("nr3",   |b| { radio[0].nr3    = b; });
on_change("nb",    |b| { radio[0].nb     = b; });
on_change("muted", |b| { radio[0].muted  = b; });
