use std::{
    fs::File,
    path::{Path, PathBuf},
};

use super::{ArchiveReader, ArchiveWriter};

pub struct DirArchive {
    path: PathBuf,
}

impl DirArchive {
    pub fn new<S: AsRef<Path>>(path: S) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
    pub fn exists<S: AsRef<Path>>(&self, path: S) -> bool {
        self.path.join(path).exists()
    }
}

impl ArchiveWriter for DirArchive {
    fn get_writer<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Write + 'this>> {
        let path = self.path.join(filename);
        Ok(Box::new(File::create(path)?))
    }
}

impl ArchiveReader for DirArchive {
    fn get_reader<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Read + 'this>> {
        let path = self.path.join(filename);
        Ok(Box::new(File::open(path)?))
    }
}
