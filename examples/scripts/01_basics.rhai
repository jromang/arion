// 01_basics.rhai — the basics of Arion scripting.
//
// Run from the "Scripts" window (menu View > Scripts):
// open this file with "Open", then click "Run".
//
// Goal: connect to the radio, tune a 20 m FT8 frequency in USB,
// set a moderate volume.

// Open the HPSDR session to the configured radio (see the Setup
// window or the HL2_IP environment variable at launch).
radio.connect();

// Set up RX 0. Writing a property uses the `radio[0]` indexer,
// because Rhai does not accept assignment on the left of a method
// result (`radio.rx(0).freq = …` is invalid).
radio[0].freq = 14074000;   // 14.074 MHz — international FT8 frequency
radio[0].mode = "USB";
radio[0].volume = 0.4;

// Print a few properties in the REPL.
print("connected: " + radio.connected);
print("RX0 on  : " + radio.rx(0).freq + " Hz, mode " + radio.rx(0).mode);
