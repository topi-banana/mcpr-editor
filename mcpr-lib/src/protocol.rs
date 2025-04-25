use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io;

pub trait Deserializer: io::Read {
    fn read_bool(&mut self) -> io::Result<bool> {
        Ok(self.read_u8()? == 1)
    }
    fn read_byte(&mut self) -> io::Result<i8> {
        self.read_i8()
    }
    fn read_unsigned_byte(&mut self) -> io::Result<u8> {
        self.read_u8()
    }
    fn read_short(&mut self) -> io::Result<i16> {
        self.read_i16::<BigEndian>()
    }
    fn read_unsigned_short(&mut self) -> io::Result<u16> {
        self.read_u16::<BigEndian>()
    }
    fn read_int(&mut self) -> io::Result<i32> {
        self.read_i32::<BigEndian>()
    }
    fn read_long(&mut self) -> io::Result<i64> {
        self.read_i64::<BigEndian>()
    }
    fn read_float(&mut self) -> io::Result<f32> {
        self.read_f32::<BigEndian>()
    }
    fn read_double(&mut self) -> io::Result<f64> {
        self.read_f64::<BigEndian>()
    }
    fn read_string(&mut self) -> io::Result<String> {
        let length = self.read_varint()? as usize;
        let mut buffer = vec![0u8; length];
        self.read_exact(&mut buffer)?;

        let s = String::from_utf8(buffer)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 string"))?
            .to_string();
        Ok(s)
    }
    fn read_varint(&mut self) -> io::Result<i32> {
        let mut val = 0;
        for i in 0..5 {
            let byte = self.read_u8()?;
            val |= (i32::from(byte) & 0x7F) << (i * 7);
            if byte & 0x80 == 0 {
                return Ok(val);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VarInt is too big",
        ))
    }
    fn read_varlong(&mut self) -> io::Result<i64> {
        let mut val = 0;
        for i in 0..10 {
            let byte = self.read_u8()?;
            val |= (i64::from(byte) & 0b01111111) << (i * 7);
            if byte & 0b10000000 == 0 {
                return Ok(val);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VarLong is too big",
        ))
    }
    fn read_uuid(&mut self) -> io::Result<uuid::Uuid> {
        let mut buffer = [0u8; 16];
        self.read_exact(&mut buffer)?;
        Ok(uuid::Uuid::from_bytes(buffer))
    }
}

impl<R: io::Read + ?Sized> Deserializer for R {}


pub trait Serializer: io::Write {
    fn write_varint(&mut self, value: i32) -> io::Result<()> {
/*
        const SEGMENT_BITS: i32 = 0x7F;
        const CONTINUE_BIT: i32 = 0x80;

        let mut val = 0;
        for i in 0..5 {
            let byte = self.read_u8()? as i32;

            val |= (byte & SEGMENT_BITS) << (7 * i);
            if byte & CONTINUE_BIT == 0 {
                return Ok(val);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VarInt is too big",
        ))
*/
        let mut val = value;
        for _ in 0..5 {
            let b: u8 = val as u8 & 0b01111111;
            val >>= 7;
            self.write_u8(if val == 0 { b } else { b | 0b10000000 })?;
            if val == 0 {
                break;
            }
        }
        Ok(())
    }
}
impl<W: io::Write + ?Sized> Serializer for W {}

/*
// Helper functions for reading and writing data

// Boolean
pub fn read_bool(cursor: &mut Cursor<&[u8]>) -> io::Result<bool> {
    let byte = cursor.read_u8()?;
    Ok(byte == 0x01)
}

pub fn write_bool<W: Write>(writer: &mut W, value: bool) -> io::Result<()> {
    writer.write_u8(if value { 0x01 } else { 0x00 })
}

// Byte
pub fn read_byte(cursor: &mut Cursor<&[u8]>) -> io::Result<i8> {
    cursor.read_i8()
}

pub fn write_byte<W: Write>(writer: &mut W, value: i8) -> io::Result<()> {
    writer.write_i8(value)
}

// Unsigned Byte
pub fn read_unsigned_byte(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
    cursor.read_u8()
}

pub fn write_unsigned_byte<W: Write>(writer: &mut W, value: u8) -> io::Result<()> {
    writer.write_u8(value)
}

// Short
pub fn read_short(cursor: &mut Cursor<&[u8]>) -> io::Result<i16> {
    cursor.read_i16::<BigEndian>()
}

pub fn write_short<W: Write>(writer: &mut W, value: i16) -> io::Result<()> {
    writer.write_i16::<BigEndian>(value)
}

// Unsigned Short
pub fn read_unsigned_short(cursor: &mut Cursor<&[u8]>) -> io::Result<u16> {
    cursor.read_u16::<BigEndian>()
}

pub fn write_unsigned_short<W: Write>(writer: &mut W, value: u16) -> io::Result<()> {
    writer.write_u16::<BigEndian>(value)
}

// Int
pub fn read_int(cursor: &mut Cursor<&[u8]>) -> io::Result<i32> {
    cursor.read_i32::<BigEndian>()
}

pub fn write_int<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_i32::<BigEndian>(value)
}

// Long
pub fn read_long(cursor: &mut Cursor<&[u8]>) -> io::Result<i64> {
    cursor.read_i64::<BigEndian>()
}

pub fn write_long<W: Write>(writer: &mut W, value: i64) -> io::Result<()> {
    writer.write_i64::<BigEndian>(value)
}

// Float
pub fn read_float(cursor: &mut Cursor<&[u8]>) -> io::Result<f32> {
    cursor.read_f32::<BigEndian>()
}

pub fn write_float<W: Write>(writer: &mut W, value: f32) -> io::Result<()> {
    writer.write_f32::<BigEndian>(value)
}

// Double
pub fn read_double(cursor: &mut Cursor<&[u8]>) -> io::Result<f64> {
    cursor.read_f64::<BigEndian>()
}

pub fn write_double<W: Write>(writer: &mut W, value: f64) -> io::Result<()> {
    writer.write_f64::<BigEndian>(value)
}

// String
pub fn read_string(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
    let length = read_varint(cursor)?;
    let mut buffer = vec![0u8; length as usize];
    cursor.read_exact(&mut buffer)?;
    String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_string<W: Write>(writer: &mut W, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    write_varint(writer, bytes.len() as i32)?;
    writer.write_all(bytes)
}

// VarInt
pub fn read_varint(cursor: &mut Cursor<&[u8]>) -> io::Result<i32> {
    let mut value: i32 = 0;
    let mut position: i32 = 0;
    let mut current_byte: u8;

    loop {
        current_byte = cursor.read_u8()?;
        value |= ((current_byte & 0x7F) as i32) << position;

        if (current_byte & 0x80) == 0 {
            break;
        }

        position += 7;

        if position >= 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VarInt is too big",
            ));
        }
    }

    Ok(value)
}

pub fn write_varint<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    let mut value = value as u32; // Treat as unsigned for bit manipulation

    loop {
        if (value & !0x7F) == 0 {
            writer.write_u8(value as u8)?;
            return Ok(());
        }

        writer.write_u8(((value & 0x7F) | 0x80) as u8)?;
        value >>= 7;
    }
}


// VarLong
pub fn read_varlong(cursor: &mut Cursor<&[u8]>) -> io::Result<i64> {
    let mut value: i64 = 0;
    let mut position: i32 = 0;
    let mut current_byte: u8;

    loop {
        current_byte = cursor.read_u8()?;
        value |= ((current_byte & 0x7F) as i64) << position;

        if (current_byte & 0x80) == 0 {
            break;
        }

        position += 7;

        if position >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VarLong is too big",
            ));
        }
    }

    Ok(value)
}

pub fn write_varlong<W: Write>(writer: &mut W, value: i64) -> io::Result<()> {
    let mut value = value as u64; // Treat as unsigned for bit manipulation

    loop {
        if (value & !0x7F) == 0 {
            writer.write_u8(value as u8)?;
            return Ok(());
        }

        writer.write_u8(((value & 0x7F) | 0x80) as u8)?;
        value >>= 7;
    }
}

// Position
pub fn read_position(cursor: &mut Cursor<&[u8]>) -> io::Result<(i32, i32, i32)> {
    let val = cursor.read_i64::<BigEndian>()?;
    let x = (val >> 38) as i32;
    let y = (val << 52 >> 52) as i32;
    let z = (val << 26 >> 38) as i32;
    Ok((x, y, z))
}

pub fn write_position<W: Write>(writer: &mut W, x: i32, y: i32, z: i32) -> io::Result<()> {
    let val = (((x as i64 & 0x3FFFFFF) << 38) | ((z as i64 & 0x3FFFFFF) << 12) | (y as i64 & 0xFFF)) as i64;
    writer.write_i64::<BigEndian>(val)
}


// Angle
pub fn read_angle(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
    cursor.read_u8()
}

pub fn write_angle<W: Write>(writer: &mut W, value: u8) -> io::Result<()> {
    writer.write_u8(value)
}

// UUID
pub fn read_uuid(cursor: &mut Cursor<&[u8]>) -> io::Result<uuid::Uuid> {
    let mut buffer = [0u8; 16];
    cursor.read_exact(&mut buffer)?;
    uuid::Uuid::from_bytes(buffer).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UUID"))
}

pub fn write_uuid<W: Write>(writer: &mut W, value: &uuid::Uuid) -> io::Result<()> {
    writer.write_all(value.as_bytes())
}

// Prefixed Array
pub fn read_prefixed_array<T, F>(cursor: &mut Cursor<&[u8]>, read_element: F) -> io::Result<Vec<T>>
where
    F: Fn(&mut Cursor<&[u8]>) -> io::Result<T>,
{
    let length = read_varint(cursor)?;
    let mut result = Vec::with_capacity(length as usize);
    for _ in 0..length {
        result.push(read_element(cursor)?);
    }
    Ok(result)
}

pub fn write_prefixed_array<T, F, W: Write>(writer: &mut W, array: &[T], write_element: F) -> io::Result<()>
where
    F: Fn(&mut W, &T) -> io::Result<()>,
{
    write_varint(writer, array.len() as i32)?;
    for element in array {
        write_element(writer, element)?;
    }
    Ok(())
}

// BitSet
pub fn read_bitset(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<u64>> {
    let length = read_varint(cursor)?;
    let mut data = vec![0u64; length as usize];
    for i in 0..length as usize {
        data[i] = cursor.read_u64::<BigEndian>()?;
    }
    Ok(data)
}

pub fn write_bitset<W: Write>(writer: &mut W, bitset: &[u64]) -> io::Result<()> {
    write_varint(writer, bitset.len() as i32)?;
    for &long in bitset {
        writer.write_u64::<BigEndian>(long)?;
    }
    Ok(())
}

// Fixed BitSet
pub fn read_fixed_bitset(cursor: &mut Cursor<&[u8]>, n: usize) -> io::Result<Vec<u8>> {
    let length = (n as f64 / 8.0).ceil() as usize;
    let mut data = vec![0u8; length];
    cursor.read_exact(&mut data)?;
    Ok(data)
}

pub fn write_fixed_bitset<W: Write>(writer: &mut W, bitset: &[u8], n: usize) -> io::Result<()> {
    let length = (n as f64 / 8.0).ceil() as usize;
    let mut padded_bitset = bitset.to_vec();
    padded_bitset.resize(length, 0);
    writer.write_all(&padded_bitset)
}

// Optional X
pub fn read_optional<T, F>(cursor: &mut Cursor<&[u8]>, read_value: F) -> io::Result<Option<T>>
where
    F: Fn(&mut Cursor<&[u8]>) -> io::Result<T>,
{
    let present = read_bool(cursor)?;
    if present {
        Ok(Some(read_value(cursor)?))
    } else {
        Ok(None)
    }
}

pub fn write_optional<T, F, W: Write>(writer: &mut W, value: &Option<T>, write_value: F) -> io::Result<()>
where
    F: Fn(&mut W, &T) -> io::Result<()>,
{
    if let Some(val) = value {
        write_bool(writer, true)?;
        write_value(writer, val)?;
    } else {
        write_bool(writer, false)?;
    }
    Ok(())
}

// ID or X

pub fn read_id_or_x<T, F>(cursor: &mut Cursor<&[u8]>, read_x: F) -> io::Result<Result<i32, T>>
where
    F: Fn(&mut Cursor<&[u8]>) -> io::Result<T>,
{
    let id = read_varint(cursor)?;
    if id == 0 {
        let x = read_x(cursor)?;
        Ok(Result::Err(x))
    } else {
        Ok(Result::Ok(id - 1))
    }
}

pub fn write_id_or_x<T, F, W: Write>(writer: &mut W, value: &Result<i32, T>, write_x: F) -> io::Result<()>
where
    F: Fn(&mut W, &T) -> io::Result<()>,
{
    match value {
        Result::Ok(id) => {
            write_varint(writer, id + 1)?;
        }
        Result::Err(x) => {
            write_varint(writer, 0)?;
            write_x(writer, x)?;
        }
    }
    Ok(())
}

//ID Set
pub fn read_id_set(cursor: &mut Cursor<&[u8]>) -> io::Result<Result<String, Vec<i32>>> {
    let type_value = read_varint(cursor)?;

    if type_value == 0 {
        let tag_name = read_string(cursor)?;
        Ok(Result::Ok(tag_name))
    } else {
        let length = type_value - 1;
        let mut ids = Vec::with_capacity(length as usize);
        for _ in 0..length {
            ids.push(read_varint(cursor)?);
        }
        Ok(Result::Err(ids))
    }
}

pub fn write_id_set<W: Write>(writer: &mut W, value: &Result<String, Vec<i32>>) -> io::Result<()> {
    match value {
        Result::Ok(tag_name) => {
            write_varint(writer, 0)?;
            write_string(writer, tag_name)?;
        }
        Result::Err(ids) => {
            write_varint(writer, (ids.len() + 1) as i32)?;
            for &id in ids {
                write_varint(writer, id)?;
            }
        }
    }
    Ok(())
}

// Sound Event
#[derive(Debug, PartialEq)]
pub struct SoundEvent {
    pub sound_name: String,
    pub has_fixed_range: bool,
    pub fixed_range: Option<f32>,
}

pub fn read_sound_event(cursor: &mut Cursor<&[u8]>) -> io::Result<SoundEvent> {
    let sound_name = read_string(cursor)?;
    let has_fixed_range = read_bool(cursor)?;
    let fixed_range = if has_fixed_range {
        Some(read_float(cursor)?)
    } else {
        None
    };

    Ok(SoundEvent {
        sound_name,
        has_fixed_range,
        fixed_range,
    })
}

pub fn write_sound_event<W: Write>(writer: &mut W, sound_event: &SoundEvent) -> io::Result<()> {
    write_string(writer, &sound_event.sound_name)?;
    write_bool(writer, sound_event.has_fixed_range)?;
    if sound_event.has_fixed_range {
        if let Some(fixed_range) = sound_event.fixed_range {
            write_float(writer, fixed_range)?;
        }
    }
    Ok(())
}

// Chat Type (Example Structure)
#[derive(Debug, PartialEq)]
pub struct ChatType {
    // The actual structure depends on the specific Minecraft version
    // This is a simplified example
    pub translation_key: String,
    pub parameters: Vec<i32>,
    //  pub style: NBT // NBT needs a more complex implementation
}

pub fn read_chat_type(cursor: &mut Cursor<&[u8]>) -> io::Result<ChatType> {
    let translation_key = read_string(cursor)?;
    let parameters = read_prefixed_array(cursor, |c| read_varint(c))?;
    //let style = read_nbt(cursor)?; // NBT implementation is needed

    Ok(ChatType {
        translation_key,
        parameters,
        //style,
    })
}

pub fn write_chat_type<W: Write>(writer: &mut W, chat_type: &ChatType) -> io::Result<()> {
    write_string(writer, &chat_type.translation_key)?;
    write_prefixed_array(writer, &chat_type.parameters, |w, &p| write_varint(w, p))?;
    //write_nbt(writer, &chat_type.style)?; // NBT implementation is needed

    Ok(())
}

// Teleport Flags
pub fn read_teleport_flags(cursor: &mut Cursor<&[u8]>) -> io::Result<i32> {
    read_int(cursor)
}

pub fn write_teleport_flags<W: Write>(writer: &mut W, flags: i32) -> io::Result<()> {
    write_int(writer, flags)
}


// Chunk Data and Light Data would require more complex NBT and chunk format parsing
// These are placeholders:

// Chunk Data (Placeholder)
pub fn read_chunk_data(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<u8>> {
    // Replace with actual chunk data parsing logic
    let mut data = Vec::new();
    cursor.read_to_end(&mut data)?; // Read remaining bytes as chunk data
    Ok(data)
}

pub fn write_chunk_data<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    // Replace with actual chunk data writing logic
    writer.write_all(data)
}

// Light Data (Placeholder)
pub fn read_light_data(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<u8>> {
    // Replace with actual light data parsing logic
    let mut data = Vec::new();
    cursor.read_to_end(&mut data)?; // Read remaining bytes as light data
    Ok(data)
}

pub fn write_light_data<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    // Replace with actual light data writing logic
    writer.write_all(data)
}

// Identifier validation
pub fn is_valid_identifier_namespace(namespace: &str) -> bool {
    namespace.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

pub fn is_valid_identifier_value(value: &str) -> bool {
    value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_' || c == '/')
}

pub fn validate_identifier(identifier: &str) -> Result<(), String> {
    let parts: Vec<&str> = identifier.split(':').collect();
    match parts.len() {
        1 => {
            if !is_valid_identifier_value(parts[0]) {
                return Err("Invalid identifier value".to_string());
            }
        },
        2 => {
            if !is_valid_identifier_namespace(parts[0]) {
                return Err("Invalid identifier namespace".to_string());
            }
            if !is_valid_identifier_value(parts[1]) {
                return Err("Invalid identifier value".to_string());
            }
        },
        _ => {
            return Err("Invalid identifier format".to_string());
        }
    }
    Ok(())
}


// NBT
// This requires a separate crate and implementation.  A basic stub is below.
// You'll need to add `nbt = "0.4"` to your Cargo.toml.

#[cfg(feature = "nbt")]
pub mod nbt_impl {
    use std::io::{self, Cursor};
    use nbt::Blob;

    pub fn read_nbt(cursor: &mut Cursor<&[u8]>) -> io::Result<Blob> {
        nbt::from_reader(cursor).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn write_nbt<W: std::io::Write>(writer: &mut W, blob: &Blob) -> io::Result<()> {
        nbt::to_writer(writer, blob, None).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

#[cfg(not(feature = "nbt"))]
pub mod nbt_impl {
    use std::io::{self, Cursor};

    // Placeholder NBT structure.  Replace with actual NBT parsing.
    #[derive(Debug, PartialEq)]
    pub struct Blob {}

    pub fn read_nbt(cursor: &mut Cursor<&[u8]>) -> io::Result<Blob> {
        Err(io::Error::new(io::ErrorKind::Other, "NBT feature not enabled"))
    }

    pub fn write_nbt<W: std::io::Write>(writer: &mut W, _blob: &Blob) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Other, "NBT feature not enabled"))
    }
}

pub use nbt_impl::*;


// Example usage:
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_varint() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        write_varint(&mut buffer, 25565)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_varint(&mut cursor)?;
        assert_eq!(result, 25565);
        Ok(())
    }

    #[test]
    fn test_string() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        write_string(&mut buffer, "Hello, world!")?;

        let mut cursor = Cursor::new(buffer);
        let result = read_string(&mut cursor)?;
        assert_eq!(result, "Hello, world!".to_string());
        Ok(())
    }

    #[test]
    fn test_position() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        write_position(&mut buffer, 18357644, 831, -20882616)?;

        let mut cursor = Cursor::new(buffer);
        let (x, y, z) = read_position(&mut cursor)?;
        assert_eq!(x, 18357644);
        assert_eq!(y, 831);
        assert_eq!(z, -20882616);
        Ok(())
    }

    #[test]
    fn test_prefixed_array() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let data = vec![10, 20, 30];
        write_prefixed_array(&mut buffer, &data, |w, &x| write_varint(w, x))?;

        let mut cursor = Cursor::new(buffer);
        let result = read_prefixed_array(&mut cursor, |c| read_varint(c))?;
        assert_eq!(result, data);
        Ok(())
    }

    #[test]
    fn test_uuid() -> io::Result<()> {
        let uuid = uuid::Uuid::new_v4();
        let mut buffer: Vec<u8> = Vec::new();
        write_uuid(&mut buffer, &uuid)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_uuid(&mut cursor)?;
        assert_eq!(result, uuid);
        Ok(())
    }

    #[test]
    fn test_bitset() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let data = vec![1, 2, 3];
        write_bitset(&mut buffer, &data)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_bitset(&mut cursor)?;
        assert_eq!(result, data);
        Ok(())
    }

    #[test]
    fn test_fixed_bitset() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let data = vec![0b10101010, 0b01010101];
        let n = 16; // 16 bits
        write_fixed_bitset(&mut buffer, &data, n)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_fixed_bitset(&mut cursor, n)?;
        assert_eq!(result, data);
        Ok(())
    }

    #[test]
    fn test_optional() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let some_value = Some(12345);
        write_optional(&mut buffer, &some_value, |w, &x| write_varint(w, x))?;

        let mut cursor = Cursor::new(buffer);
        let result = read_optional(&mut cursor, |c| read_varint(c))?;
        assert_eq!(result, some_value);

        let mut buffer2: Vec<u8> = Vec::new();
        let none_value: Option<i32> = None;
        write_optional(&mut buffer2, &none_value, |w, &x| write_varint(w, x))?;

        let mut cursor2 = Cursor::new(buffer2);
        let result2 = read_optional(&mut cursor2, |c| read_varint(c))?;
        assert_eq!(result2, None);

        Ok(())
    }

    #[test]
    fn test_id_or_x() -> io::Result<()> {
        // Test with ID
        let mut buffer_id: Vec<u8> = Vec::new();
        let id_value: Result<i32, String> = Result::Ok(5);
        write_id_or_x(&mut buffer_id, &id_value, |w, x| write_string(w, x))?;

        let mut cursor_id = Cursor::new(buffer_id);
        let result_id = read_id_or_x(&mut cursor_id, |c| read_string(c))?;
        assert_eq!(result_id, Result::Ok(4)); // Adjusted for id - 1

        // Test with X
        let mut buffer_x: Vec<u8> = Vec::new();
        let x_value: Result<i32, String> = Result::Err("test".to_string());
        write_id_or_x(&mut buffer_x, &x_value, |w, x| write_string(w, x))?;

        let mut cursor_x = Cursor::new(buffer_x);
        let result_x = read_id_or_x(&mut cursor_x, |c| read_string(c))?;
        assert_eq!(result_x, Result::Err("test".to_string()));

        Ok(())
    }

    #[test]
    fn test_id_set() -> io::Result<()> {
        // Test with Tag Name
        let mut buffer_tag: Vec<u8> = Vec::new();
        let tag_name: Result<String, Vec<i32>> = Result::Ok("minecraft:blocks".to_string());
        write_id_set(&mut buffer_tag, &tag_name)?;

        let mut cursor_tag = Cursor::new(buffer_tag);
        let result_tag = read_id_set(&mut cursor_tag)?;
        assert_eq!(result_tag, Result::Ok("minecraft:blocks".to_string()));

        // Test with IDs
        let mut buffer_ids: Vec<u8> = Vec::new();
        let ids: Result<String, Vec<i32>> = Result::Err(vec![1, 2, 3]);
        write_id_set(&mut buffer_ids, &ids)?;

        let mut cursor_ids = Cursor::new(buffer_ids);
        let result_ids = read_id_set(&mut cursor_ids)?;
        assert_eq!(result_ids, Result::Err(vec![1, 2, 3]));

        Ok(())
    }

    #[test]
    fn test_sound_event() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let sound_event = SoundEvent {
            sound_name: "minecraft:entity.villager.ambient".to_string(),
            has_fixed_range: true,
            fixed_range: Some(32.0),
        };
        write_sound_event(&mut buffer, &sound_event)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_sound_event(&mut cursor)?;
        assert_eq!(result, sound_event);

        let mut buffer2: Vec<u8> = Vec::new();
        let sound_event2 = SoundEvent {
            sound_name: "minecraft:entity.cow.ambient".to_string(),
            has_fixed_range: false,
            fixed_range: None,
        };
        write_sound_event(&mut buffer2, &sound_event2)?;

        let mut cursor2 = Cursor::new(buffer2);
        let result2 = read_sound_event(&mut cursor2)?;
        assert_eq!(result2, sound_event2);

        Ok(())
    }

    #[test]
    fn test_chat_type() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let chat_type = ChatType {
            translation_key: "chat.type.announcement".to_string(),
            parameters: vec![1, 2, 3],
            //style: Blob::new(), // Requires proper NBT setup
        };
        write_chat_type(&mut buffer, &chat_type)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_chat_type(&mut cursor)?;
        assert_eq!(result.translation_key, chat_type.translation_key);
        assert_eq!(result.parameters, chat_type.parameters);
        //assert_eq!(result.style, chat_type.style);

        Ok(())
    }

    #[test]
    fn test_teleport_flags() -> io::Result<()> {
        let mut buffer: Vec<u8> = Vec::new();
        let flags = 0x000A; // Example flags (relative Z and relative Yaw)
        write_teleport_flags(&mut buffer, flags)?;

        let mut cursor = Cursor::new(buffer);
        let result = read_teleport_flags(&mut cursor)?;
        assert_eq!(result, flags);

        Ok(())
    }

    #[test]
    fn test_identifier_validation() {
        assert_eq!(is_valid_identifier_namespace("minecraft"), true);
        assert_eq!(is_valid_identifier_namespace("my_mod"), true);
        assert_eq!(is_valid_identifier_namespace("my.mod"), true);
        assert_eq!(is_valid_identifier_namespace("my-mod"), true);
        assert_eq!(is_valid_identifier_namespace("MyMod"), false); // Uppercase not allowed
        assert_eq!(is_valid_identifier_namespace(""), true); // Empty namespace allowed?

        assert_eq!(is_valid_identifier_value("item"), true);
        assert_eq!(is_valid_identifier_value("my_item"), true);
        assert_eq!(is_valid_identifier_value("my.item"), true);
        assert_eq!(is_valid_identifier_value("my-item"), true);
        assert_eq!(is_valid_identifier_value("my/item"), true);
        assert_eq!(is_valid_identifier_value("MyItem"), false); // Uppercase not allowed

        assert_eq!(validate_identifier("minecraft:item").is_ok(), true);
        assert_eq!(validate_identifier("item").is_ok(), true);
        assert_eq!(validate_identifier("my_mod:my_item").is_ok(), true);
        assert_eq!(validate_identifier("MyMod:my_item").is_err(), true);
        assert_eq!(validate_identifier("minecraft:MyItem").is_err(), true);
    }
}










*/
