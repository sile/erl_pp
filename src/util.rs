use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn substitute_path_variables<P: AsRef<Path>>(path: P) -> PathBuf {
    let mut new = PathBuf::new();
    for (i, c) in path.as_ref().components().enumerate() {
        if let (0, Some(s)) = (i, c.as_os_str().to_str()) {
            if s.as_bytes().get(0) == Some(&b'$') {
                if let Ok(c) = env::var(s.split_at(1).1) {
                    new.push(c);
                    continue;
                }
            }
        }
        new.push(c.as_os_str());
    }
    new
}

pub fn read_file<P: AsRef<Path>>(path: P) -> std::io::Result<String> {
    let mut buf = String::new();
    let mut file = File::open(&path)?;
    file.read_to_string(&mut buf)?;
    Ok(buf)
}
