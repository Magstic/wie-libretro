use std::{fs, path::PathBuf};

use wie_backend::RecordId;

pub struct LibretroDatabaseRepository {
    base_path: PathBuf,
}

impl LibretroDatabaseRepository {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    fn path_for_database(&self, name: &str, app_id: &str) -> PathBuf {
        let sanitized_app_id: String = app_id.chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
        let app_id = if sanitized_app_id.is_empty() || sanitized_app_id == "." || sanitized_app_id == ".." {
            "_"
        } else {
            &sanitized_app_id
        };

        let name: String = name.chars().map(|c| if matches!(c, '\\' | '\0') { '_' } else { c }).collect();
        let mut normalized_name = PathBuf::new();
        for segment in name.trim_start_matches('/').split('/') {
            match segment {
                "" | "." => {}
                ".." => normalized_name.push("_"),
                segment => normalized_name.push(segment),
            }
        }
        if normalized_name.as_os_str().is_empty() {
            normalized_name.push("_");
        }

        self.base_path.join(app_id).join("db").join(normalized_name)
    }
}

#[async_trait::async_trait]
impl wie_backend::DatabaseRepository for LibretroDatabaseRepository {
    async fn open(&self, name: &str, app_id: &str) -> Box<dyn wie_backend::Database> {
        let path = self.path_for_database(name, app_id);
        Box::new(LibretroDatabase::new(path))
    }

    async fn exists(&self, name: &str, app_id: &str) -> bool {
        self.path_for_database(name, app_id).is_dir()
    }

    async fn delete(&self, name: &str, app_id: &str) -> bool {
        let path = self.path_for_database(name, app_id);
        match fs::remove_dir_all(path) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => {
                tracing::warn!(error = %err, "failed to delete database");
                false
            }
        }
    }
}

pub struct LibretroDatabase {
    base_path: PathBuf,
}

impl LibretroDatabase {
    fn new(base_path: PathBuf) -> Self {
        if let Err(err) = fs::create_dir_all(&base_path) {
            tracing::warn!(?base_path, error = %err, "failed to create database directory");
        }

        Self { base_path }
    }

    fn next_free_record_id(&self) -> RecordId {
        let mut record_id = 1;
        while self.path_for_record(record_id).exists() {
            record_id += 1;
        }
        record_id
    }

    fn path_for_record(&self, id: RecordId) -> PathBuf {
        self.base_path.join(id.to_string())
    }
}

#[async_trait::async_trait]
impl wie_backend::Database for LibretroDatabase {
    async fn next_id(&self) -> RecordId {
        self.next_free_record_id()
    }

    async fn add(&mut self, data: &[u8]) -> RecordId {
        let id = self.next_free_record_id();
        if let Err(err) = fs::create_dir_all(&self.base_path) {
            tracing::warn!(?self.base_path, error = %err, "failed to create database directory before add");
            return 0;
        }
        if let Err(err) = fs::write(self.path_for_record(id), data) {
            tracing::warn!(?self.base_path, id, error = %err, "failed to add database record");
            return 0;
        }

        id
    }

    async fn get(&self, id: RecordId) -> Option<Vec<u8>> {
        match fs::read(self.path_for_record(id)) {
            Ok(data) => Some(data),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!(?self.base_path, id, error = %err, "failed to read database record");
                None
            }
        }
    }

    async fn set(&mut self, id: RecordId, data: &[u8]) -> bool {
        if let Err(err) = fs::create_dir_all(&self.base_path) {
            tracing::warn!(?self.base_path, error = %err, "failed to create database directory before set");
            return false;
        }
        match fs::write(self.path_for_record(id), data) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(?self.base_path, id, error = %err, "failed to set database record");
                false
            }
        }
    }

    async fn delete(&mut self, id: RecordId) -> bool {
        match fs::remove_file(self.path_for_record(id)) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => {
                tracing::warn!(?self.base_path, id, error = %err, "failed to delete database record");
                false
            }
        }
    }

    async fn get_record_ids(&self) -> Vec<RecordId> {
        let Ok(entries) = fs::read_dir(&self.base_path) else {
            return Vec::new();
        };

        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| entry.file_name().to_str().and_then(|name| name.parse::<RecordId>().ok()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::LibretroDatabaseRepository;

    #[test]
    fn database_path_stays_inside_app_scope() {
        let repo = LibretroDatabaseRepository::new(PathBuf::from("/tmp/wie"));
        let path = repo.path_for_database("/../save0.dat", "PD140106");

        assert!(path.starts_with(PathBuf::from("/tmp/wie/PD140106/db")));
    }
}
