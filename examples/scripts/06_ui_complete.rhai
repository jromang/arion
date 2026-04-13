// 06_ui_complete.rhai — full example combining a window, widgets,
// on_change and menu. Designed as a base for a personal startup.rhai.
//
// Copy-paste the contents into ~/.config/arion/startup.rhai so it
// gets loaded automatically at every launch.

// === 1. DSP panel =============================================
window("dsp-panel", "DSP — quick", || {
    label("Common presets for RX 0.");

    hbox(|| {
        button("FT8 80m",  || { radio[0].freq = 3573000;  radio[0].mode = "USB"; });
        button("FT8 40m",  || { radio[0].freq = 7074000;  radio[0].mode = "USB"; });
        button("FT8 20m",  || { radio[0].freq = 14074000; radio[0].mode = "USB"; });
    });

    separator();

    slider("Volume",        "vol",      0.0, 1.0);
    slider("Filter low",    "filt_lo",  0.0, 5000.0);
    slider("Filter high",   "filt_hi",  0.0, 5000.0);

    separator();

    checkbox("NR3 (RNNoise)",    "nr3");
    checkbox("NR4 (specbleach)", "nr4");
    checkbox("ANF",              "anf");
    checkbox("Muted",            "muted");
});

on_change("vol",     |v| { radio[0].volume    = v; });
on_change("filt_lo", |v| { radio[0].filter_lo = v; });
on_change("filt_hi", |v| { radio[0].filter_hi = v; });
on_change("nr3",     |b| { radio[0].nr3       = b; });
on_change("nr4",     |b| { radio[0].nr4       = b; });
on_change("anf",     |b| { radio[0].anf       = b; });
on_change("muted",   |b| { radio[0].muted     = b; });

// === 2. Quick menus ===========================================
menu_item("Arion/Show DSP",    || { window_show("dsp-panel");    });
menu_item("Arion/Hide DSP",    || { window_hide("dsp-panel");    });
menu_item("Arion/Toggle DSP",  || { window_toggle("dsp-panel");  });

menu_item("Quick bands/80m",  || { band("80"); });
menu_item("Quick bands/40m",  || { band("40"); });
menu_item("Quick bands/20m",  || { band("20"); });
menu_item("Quick bands/15m",  || { band("15"); });
menu_item("Quick bands/10m",  || { band("10"); });

// === 3. Small session shortcuts ===============================
menu_item("Session/Connect",     || { radio.connect();    });
menu_item("Session/Disconnect",  || { radio.disconnect(); });
menu_item("Session/Save",        || { radio.save();       });
