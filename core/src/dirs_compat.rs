use std::path::PathBuf;

pub fn data_dir() -> Option<PathBuf> {
    if let Ok(data_dir) = std::env::var("XDG_DATA_HOME") {
        if !data_dir.is_empty() {
            return Some(PathBuf::from(data_dir));
        }
    }
    
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".local").join("share"));
    }
    
    None
}