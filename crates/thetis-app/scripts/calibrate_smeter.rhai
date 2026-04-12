// calibrate_smeter.rhai — S-meter calibration helper
//
// Tunes RX0 to WWV 10 MHz (AM), reads the S-meter after a short
// settle time, and prints the suggested calibration offset.
//
// WWV broadcasts a continuous carrier at known power levels.
// At 10 MHz, a reasonable signal in most of North America / Europe
// should read approximately S9 (-73 dBm). The difference between
// the measured value and -73 dBm is the calibration offset for
// the current band.
//
// Usage in the REPL:
//   1. Make sure you're connected to the HL2
//   2. Paste or run this script
//   3. Note the suggested offset
//   4. Go to Setup → Calibration and enter the offset for 30m
//      (WWV 10 MHz falls in the 30m band)
//
// Note: this is a manual helper, not automatic. True auto-cal
// would require averaging over multiple readings and handling
// propagation variations — that's a future enhancement.

// Tune to WWV 10 MHz AM
freq(0, 10000000);
mode(0, "AM");

print("Tuned to WWV 10.000 MHz AM");
print("Wait a few seconds for AGC to settle, then read:");
print("");
print("Current S-meter: " + rx0_smeter + " dBFS");

let dbm = rx0_smeter - 73.0;
print("Estimated dBm:   " + dbm);
print("");

let expected_s9 = -73.0;
let offset = expected_s9 - dbm;
print("If WWV is truly S9 at your location:");
print("  Suggested calibration offset for 30m: " + offset + " dB");
print("");
print("Enter this in Setup → Calibration → 30m band.");
