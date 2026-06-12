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
            // default() は wasm で未実装の SystemTime::now() を呼ぶため、
            // mtime 固定の DEFAULT から組み立てる。
            option: SimpleFileOptions::DEFAULT
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(compression_level),
        }
    }

    /// アーカイブを finalize して内側の writer を取り戻す。
    /// in-memory 書き出し (`Cursor<Vec<u8>>`) でバイト列を回収するために使う。
    pub fn finish(self) -> zip::result::ZipResult<W> {
        self.zip.finish()
    }
}

impl<W: Write + Seek> ArchiveWriter for ZipArchiveWriter<W> {
    fn get_writer<'this>(
        &'this mut self,
        filename: &str,
    ) -> anyhow::Result<Box<dyn Write + 'this>> {
        self.zip.start_file(filename, self.option)?;
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn write_archive() -> Vec<u8> {
        let mut writer = ZipArchiveWriter::new(Cursor::new(Vec::new()), None);
        writer
            .get_writer("a.txt")
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        writer
            .get_writer("dir/b.bin")
            .unwrap()
            .write_all(&[0u8; 256])
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn finish_roundtrips_entries() {
        let bytes = write_archive();
        let mut reader = ZipArchiveReader::new(Cursor::new(bytes)).unwrap();
        let mut a = Vec::new();
        reader
            .get_reader("a.txt")
            .unwrap()
            .read_to_end(&mut a)
            .unwrap();
        assert_eq!(a, b"hello");
        let mut b = Vec::new();
        reader
            .get_reader("dir/b.bin")
            .unwrap()
            .read_to_end(&mut b)
            .unwrap();
        assert_eq!(b, vec![0u8; 256]);
    }

    #[test]
    fn output_is_deterministic() {
        // mtime を固定しているため同一入力からの出力はバイト単位で一致する。
        assert_eq!(write_archive(), write_archive());
    }
}
