//! Names and places: the environment variables (`ATLAS_*`; the earlier `CODOL_ASSETS_*` names are still read) and the
//! per-user folders for settings and caches. The program was called codol-assets before it was Atlas: a folder left under
//! the old name is copied to the new one the first time it is needed (the old one is left alone).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `ATLAS_<name>`, else `CODOL_ASSETS_<name>`.
pub fn env_var_os(name: &str) -> Option<OsString> {
    std::env::var_os(format!("ATLAS_{name}")).or_else(|| std::env::var_os(format!("CODOL_ASSETS_{name}")))
}

pub fn env_var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("ATLAS_{name}")).or_else(|_| std::env::var(format!("CODOL_ASSETS_{name}")))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// `<base>/atlas`, filled from `<base>/codol-assets` when only that exists.
fn user_dir(base: Option<PathBuf>) -> Option<PathBuf> {
    let base = base?;
    let (new, old) = (base.join("atlas"), base.join("codol-assets"));
    if !new.exists() && old.is_dir() {
        let _ = copy_dir(&old, &new);
    }
    Some(new)
}

/// The folder for settings and remembered paths.
pub fn config_dir() -> Option<PathBuf> {
    user_dir(dirs::config_dir())
}

/// The folder for caches that can be deleted at any time.
pub fn cache_dir() -> Option<PathBuf> {
    user_dir(dirs::cache_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_folder_is_copied_once_and_left_in_place() {
        let base = std::env::temp_dir().join("atlas_config_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("codol-assets").join("sub")).unwrap();
        std::fs::write(base.join("codol-assets").join("settings.json"), "{}").unwrap();
        std::fs::write(base.join("codol-assets").join("sub").join("x"), "1").unwrap();
        let dir = user_dir(Some(base.clone())).unwrap();
        assert_eq!(dir, base.join("atlas"));
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), "{}");
        assert_eq!(std::fs::read_to_string(dir.join("sub").join("x")).unwrap(), "1");
        assert!(base.join("codol-assets").join("settings.json").exists());
        // a second call must not copy over what has changed since
        std::fs::write(dir.join("settings.json"), "{\"a\":1}").unwrap();
        user_dir(Some(base.clone()));
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), "{\"a\":1}");
        let _ = std::fs::remove_dir_all(base);
    }
}
