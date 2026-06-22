use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

use wie_backend::Filesystem;

pub struct LibretroFilesystem {
    base_path: PathBuf,
}

impl LibretroFilesystem {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    fn path_for(&self, aid: &str, path: &str) -> Option<PathBuf> {
        let sanitized_aid: String = aid.chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
        if sanitized_aid.is_empty() || sanitized_aid == "." || sanitized_aid == ".." {
            tracing::error!(aid, path, "rejected filesystem path with invalid aid");
            return None;
        }

        let mut normalized = PathBuf::new();
        for component in Path::new(path).components() {
            match component {
                Component::Normal(component) => normalized.push(component),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    tracing::error!(aid, path, "rejected filesystem path traversal attempt");
                    return None;
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            tracing::error!(aid, path, "rejected empty filesystem path");
            return None;
        }

        Some(self.base_path.join(sanitized_aid).join("fs").join(normalized))
    }
}

#[async_trait::async_trait]
impl Filesystem for LibretroFilesystem {
    async fn exists(&self, aid: &str, path: &str) -> bool {
        let Some(disk_path) = self.path_for(aid, path) else {
            return false;
        };

        disk_path.metadata().map(|metadata| metadata.is_file()).unwrap_or(false)
    }

    async fn size(&self, aid: &str, path: &str) -> Option<usize> {
        let disk_path = self.path_for(aid, path)?;
        let metadata = disk_path.metadata().ok()?;
        metadata.is_file().then_some(metadata.len() as usize)
    }

    async fn read(&self, aid: &str, path: &str, offset: usize, count: usize, buf: &mut [u8]) -> Option<usize> {
        let disk_path = self.path_for(aid, path)?;
        let mut file = match OpenOptions::new().read(true).open(&disk_path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!(aid, path, error = %err, "failed to open file for reading");
                return None;
            }
        };

        let size = file.metadata().map(|metadata| metadata.len() as usize).unwrap_or(0);
        if offset >= size {
            return Some(0);
        }

        if let Err(err) = file.seek(SeekFrom::Start(offset as u64)) {
            tracing::warn!(aid, path, error = %err, "failed to seek file for reading");
            return Some(0);
        }

        let to_read = count.min(size - offset).min(buf.len());
        match file.read_exact(&mut buf[..to_read]) {
            Ok(()) => Some(to_read),
            Err(err) => {
                tracing::warn!(aid, path, error = %err, "failed to read file");
                Some(0)
            }
        }
    }

    async fn write(&self, aid: &str, path: &str, offset: usize, data: &[u8]) -> usize {
        let Some(disk_path) = self.path_for(aid, path) else {
            return 0;
        };

        if let Some(parent) = disk_path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            tracing::warn!(aid, path, error = %err, "failed to create parent directory");
            return 0;
        }

        let mut file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&disk_path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(aid, path, error = %err, "failed to open file for writing");
                return 0;
            }
        };

        let current_size = file.metadata().map(|metadata| metadata.len() as usize).unwrap_or(0);
        if offset > current_size
            && let Err(err) = file.set_len(offset as u64)
        {
            tracing::warn!(aid, path, error = %err, "failed to extend file");
            return 0;
        }

        if let Err(err) = file.seek(SeekFrom::Start(offset as u64)) {
            tracing::warn!(aid, path, error = %err, "failed to seek file for writing");
            return 0;
        }

        match file.write_all(data) {
            Ok(()) => data.len(),
            Err(err) => {
                tracing::warn!(aid, path, error = %err, "failed to write file");
                0
            }
        }
    }

    async fn truncate(&self, aid: &str, path: &str, len: usize) {
        let Some(disk_path) = self.path_for(aid, path) else {
            return;
        };

        if let Some(parent) = disk_path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            tracing::warn!(aid, path, error = %err, "failed to create parent directory before truncate");
            return;
        }

        let file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&disk_path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(aid, path, error = %err, "failed to open file for truncate");
                return;
            }
        };

        if let Err(err) = file.set_len(len as u64) {
            tracing::warn!(aid, path, error = %err, "failed to truncate file");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::LibretroFilesystem;

    #[test]
    fn path_rejects_parent_components() {
        let fs = LibretroFilesystem::new(PathBuf::from("/tmp/wie"));

        assert!(fs.path_for("game", "../escape").is_none());
    }

    #[test]
    fn path_scopes_by_aid() {
        let fs = LibretroFilesystem::new(PathBuf::from("/tmp/wie"));

        assert_eq!(fs.path_for("game", "save.bin"), Some(PathBuf::from("/tmp/wie/game/fs/save.bin")));
    }
}
