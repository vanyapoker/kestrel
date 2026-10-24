//! Named layouts use the same version-tolerant format as the session, wrapped
//! in JSON to preserve arbitrary names without using them as filesystem paths.

use crate::session::Session;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn file() -> Option<PathBuf> {
    Some(crate::session::file()?.with_file_name("workspaces.json"))
}

pub fn load(path: &Path) -> Result<BTreeMap<String, Session>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    let values: BTreeMap<String, String> =
        serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(values
        .into_iter()
        .filter_map(|(name, text)| {
            let session = crate::session::parse(&text);
            (!name.trim().is_empty() && !session.panes.is_empty()).then_some((name, session))
        })
        .collect())
}

pub fn save(path: &Path, workspaces: &BTreeMap<String, Session>) -> Result<(), String> {
    let values: BTreeMap<_, _> = workspaces
        .iter()
        .map(|(name, session)| (name, session.to_text()))
        .collect();
    let text = serde_json::to_string_pretty(&values).map_err(|e| e.to_string())?;
    crate::session::save(path, &text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_names_are_data_and_malformed_storage_is_reported() {
        let root = std::env::temp_dir().join(format!("kestrel-workspaces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("workspaces.json");
        assert!(load(&path).unwrap().is_empty());
        let session = crate::session::parse("pane 0\ntab name asc details /tmp\nsplit_ratio 0.7\n");
        let workspaces = BTreeMap::from([("Photos / Фотографии".to_string(), session)]);
        save(&path, &workspaces).unwrap();
        assert_eq!(load(&path).unwrap(), workspaces);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::write(&path, "malformed").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "malformed");
        std::fs::remove_dir_all(root).unwrap();
    }
}
