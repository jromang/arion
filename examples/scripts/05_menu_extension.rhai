// 05_menu_extension.rhai — several menu entries for common actions.
//
// Once loaded, the "Scripts" menu in the menu bar offers:
//  - Bands/80m, /40m, /20m
//  - DSP/NR3 on, /NR3 off
//  - Session/Connect, /Disconnect, /Save

menu_item("Bands/80m",  || { band("80");  });
menu_item("Bands/40m",  || { band("40");  });
menu_item("Bands/20m",  || { band("20");  });

menu_item("DSP/NR3 on",  || { radio[0].nr3 = true;  });
menu_item("DSP/NR3 off", || { radio[0].nr3 = false; });

menu_item("Session/Connect",    || { radio.connect();    });
menu_item("Session/Disconnect", || { radio.disconnect(); });
menu_item("Session/Save",       || { radio.save();       });
