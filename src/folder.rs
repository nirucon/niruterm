//! The scratchpad's folder across restarts: NIRUWM starts the scratchpad as
//! `niruterm --class scratchpad`, and after a restart or a new login it opens
//! in the folder it had last. Only the path is kept, in
//! `$XDG_STATE_HOME/niruterm/scratchpad-folder`.
use std::path::{Path, PathBuf};

/// The window class of the NIRUWM scratchpad.
pub const SCRATCHPAD: &str = "scratchpad";

pub fn file() -> PathBuf {
    crate::palette::state_home().join("niruterm/scratchpad-folder")
}

/// The folder kept last time, when it still exists.
pub fn load(file: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let data = std::fs::read(file).ok()?;
    let line = data.strip_suffix(b"\n").unwrap_or(&data);
    let p = PathBuf::from(std::ffi::OsStr::from_bytes(line));
    (p.is_absolute() && p.is_dir()).then_some(p)
}

/// Keeps a folder, written whole so a crash never leaves half a path.
pub fn save(file: &Path, dir: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = dir.as_os_str().as_bytes();
    if !dir.is_absolute() || bytes.contains(&b'\n') {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    let mut data = bytes.to_vec();
    data.push(b'\n');
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_kept_and_a_missing_one_is_forgotten() {
        let dir = std::env::temp_dir().join(format!("niruterm-folder-{}", std::process::id()));
        let file = dir.join("state/niruterm/scratchpad-folder");
        let target = dir.join("my folder");
        std::fs::create_dir_all(&target).unwrap();
        assert_eq!(load(&file), None);
        save(&file, &target).unwrap();
        assert_eq!(load(&file), Some(target.clone()));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            format!("{}\n", target.display()).into_bytes()
        );
        save(&file, Path::new("relative")).unwrap();
        assert_eq!(load(&file), Some(target.clone()));
        std::fs::remove_dir(&target).unwrap();
        assert_eq!(load(&file), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
