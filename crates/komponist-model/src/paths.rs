//! Lexical path helpers for file links (the Sampler's samples, audio clip files).
//!
//! A link keeps the absolute path and the path relative to the project file, so a project folder that
//! moved as a whole still finds its files. None of these functions touches the file system.
//!
//! Thread: any.

use std::path::{Component, Path, PathBuf};

/// `path` as an absolute path (against the current folder), without touching the file system.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        std::env::current_dir()
            .map(|d| normalize(&d.join(path)))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// `path` without `.` and resolvable `..` parts (lexically).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` relative to the folder `base` (both absolute): `Kit/kick.wav`, `../Samples/kick.wav`.
pub fn relative(path: &Path, base: &Path) -> PathBuf {
    let path = normalize(path);
    let base = normalize(base);
    let (p, b): (Vec<_>, Vec<_>) = (path.components().collect(), base.components().collect());
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return path;
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_climb_and_descend() {
        let r = |p: &str, b: &str| relative(Path::new(p), Path::new(b));
        assert_eq!(r("/a/b/k.wav", "/a/b"), Path::new("k.wav"));
        assert_eq!(r("/a/b/S/k.wav", "/a/b"), Path::new("S/k.wav"));
        assert_eq!(r("/a/x/k.wav", "/a/b/c"), Path::new("../../x/k.wav"));
    }

    #[test]
    fn normalize_drops_dots() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), Path::new("/a/c"));
    }
}
