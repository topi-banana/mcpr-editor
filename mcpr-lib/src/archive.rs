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
