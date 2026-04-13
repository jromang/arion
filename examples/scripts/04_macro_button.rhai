// 04_macro_button.rhai — adds a "macro" entry to the Scripts menu.
//
// After running this script, the Scripts menu contains the entry
// "Favorites/FT8 20 m". Clicking it jumps to 14.074 MHz, USB,
// 2.4 kHz filter, NR3 enabled.

menu_item("Favorites/FT8 20 m", || {
    band("20");
    radio[0].freq = 14074000;
    radio[0].mode = "USB";
    filter_preset(radio.rx(0), "2.4K");
    radio[0].nr3 = true;
});
