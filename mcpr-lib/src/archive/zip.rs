use std::io::{Read, Seek, Write};

use zip::{
    ZipArchive, ZipWriter,
    result::ZipError,
    write::{FileOptions, SimpleFileOptions},
};

use super::{ArchiveReader, ArchiveWriter};

pub struct ZipArchiveWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
    option: FileOptions<'static, ()>,
}

impl<W: Write + Seek> ZipArchiveWriter<W> {
    pub fn new(writer: W, compression_level: Option<i64>) -> Self {
        Self {
            zip: ZipWriter::new(writer),
            option: SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(compression_level),
        }
    }
}

impl<W: Write + Seek> ArchiveWriter for ZipArchiveWriter<W> {
    fn get_writer<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn Write + 'this>> {
        self.zip.start_file(filename, self.option.clone())?;
        Ok(Box::new(&mut self.zip))
    }
}

pub struct ZipArchiveReader<W: Read + Seek> {
    zip: ZipArchive<W>,
}

impl<W: Read + Seek> ZipArchiveReader<W> {
    pub fn new(reader: W) -> Result<Self, ZipError> {
        Ok(Self {
            zip: ZipArchive::new(reader)?,
        })
    }
}

impl<R: Read + Seek> ArchiveReader for ZipArchiveReader<R> {
    fn get_reader<'this>(&'this mut self, filename: &str) -> anyhow::Result<Box<dyn Read + 'this>> {
        let file = self.zip.by_name(filename)?;
        Ok(Box::new(file))
    }
}
