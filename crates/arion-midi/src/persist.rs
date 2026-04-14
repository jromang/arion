use std::path::PathBuf;

use directories::ProjectDirs;

use crate::mapping::MappingTable;

/// `~/.config/arion/midi.toml` on Linux, equivalent on macOS/Windows.
pub fn midi_config_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "arion").map(|p| p.config_dir().join("midi.toml"))
}

/// Load the persisted mapping table. Returns [`MappingTable::default`]
/// (i.e. empty) if the file is missing, unreadable, or malformed —
/// a corrupted file must never prevent the app from starting.
pub fn load() -> MappingTable {
    let Some(path) = midi_config_path() else {
        return MappingTable::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return MappingTable::default();
    };
    match toml::from_str::<MappingTable>(&text) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "midi: config parse failed");
            MappingTable::default()
        }
    }
}

/// Serialize `table` to `midi.toml`. Writes to a `.tmp` sibling
/// first, then renames atomically so a SIGKILL mid-write can't
/// leave the user with an empty file.
pub fn save(table: &MappingTable) -> std::io::Result<()> {
    let Some(path) = midi_config_path() else {
        return Err(std::io::Error::other("no config dir"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(table)
        .map_err(|e| std::io::Error::other(format!("toml serialize: {e}")))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
