#[cfg(feature = "fs")]
pub mod directory;
pub mod zip;

pub trait ArchiveWriter {
    fn get_writer<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Write + 'this>>;
}

pub trait ArchiveReader {
    fn get_reader<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Read + 'this>>;
}

impl<T: ?Sized + ArchiveWriter> ArchiveWriter for Box<T> {
    fn get_writer<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Write + 'this>> {
        (**self).get_writer(filename)
    }
}

impl<T: ?Sized + ArchiveReader> ArchiveReader for Box<T> {
    fn get_reader<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn std::io::Read + 'this>> {
        (**self).get_reader(filename)
    }
}

/// crate 内 unit test 共用のメモリ上アーカイブ。
#[cfg(test)]
pub(crate) mod testing {
    use std::{
        collections::HashMap,
        io::{Cursor, Read, Write},
    };

    use super::{ArchiveReader, ArchiveWriter};

    #[derive(Default)]
    pub(crate) struct MemArchive(pub(crate) HashMap<String, Vec<u8>>);

    impl ArchiveReader for MemArchive {
        fn get_reader<'this>(
            &'this mut self,
            filename: &str,
        ) -> anyhow::Result<Box<dyn Read + 'this>> {
            let data = self
                .0
                .get(filename)
                .ok_or_else(|| anyhow::anyhow!("no such file: {}", filename))?;
            Ok(Box::new(Cursor::new(data.clone())))
        }
    }

    impl ArchiveWriter for MemArchive {
        fn get_writer<'this>(
            &'this mut self,
            filename: &str,
        ) -> anyhow::Result<Box<dyn Write + 'this>> {
            Ok(Box::new(self.0.entry(filename.to_string()).or_default()))
        }
    }
}
