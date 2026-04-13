// 02_scan.rhai — walk through a list of favourite frequencies.
//
// Rhai in Arion runs on the UI thread and has no blocking `sleep`.
// We "scan" here by setting each frequency in turn — re-run the
// script (or create one menu_item per frequency) to move to the
// next one.

let favorites = [
    #{ name: "FT8 80m",  f: 3573000  },
    #{ name: "FT8 40m",  f: 7074000  },
    #{ name: "FT8 30m",  f: 10136000 },
    #{ name: "FT8 20m",  f: 14074000 },
    #{ name: "FT8 17m",  f: 18100000 },
    #{ name: "FT8 15m",  f: 21074000 },
    #{ name: "FT8 10m",  f: 28074000 },
];

// Simple walk: print the list for reference.
print("=== Favorites ===");
for f in favorites {
    print(" - " + f.name + "\t" + f.f + " Hz");
}

// Jump directly to the first entry.
radio[0].mode  = "USB";
radio[0].freq  = favorites[0].f;
print("Tuned to " + favorites[0].name);
