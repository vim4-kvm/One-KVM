//! UU FTP RPC field numbers, separate from filesystem policy and transfer scheduling.
use super::super::wire::{self, WireField, WireValue};
use std::io::{Read, Write};

pub const OK: u64 = 1;
pub const PROTOCOL: u64 = 3;
pub const PERMISSION: u64 = 4;
pub const DISK: u64 = 5;
pub const IO: u64 = 6;
pub const CANCELED: u64 = 7;
pub const NOT_FOUND: u64 = 9;
pub const BLOCK_BYTES: usize = 32 * 1024;
pub const MAX_LIST_BYTES: usize = 512 * 1024;
pub const MAX_FILES: usize = 1024;
pub const MAX_WIRE_BYTES: usize = 60 * 1024;

pub fn error(error: &std::io::Error) -> u64 {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => PERMISSION,
        std::io::ErrorKind::InvalidInput => PROTOCOL,
        std::io::ErrorKind::NotFound => NOT_FOUND,
        _ if error.raw_os_error() == Some(libc::ENOSPC) => DISK,
        _ => IO,
    }
}

pub fn number(fields: &[WireField<'_>], field: u32) -> u64 {
    wire::number(fields, field).unwrap_or(0)
}
pub fn bytes<'a>(fields: &[WireField<'a>], field: u32) -> &'a [u8] {
    wire::message(fields, field).unwrap_or_default()
}
pub fn text<'a>(fields: &[WireField<'a>], field: u32) -> Result<&'a str, u64> {
    std::str::from_utf8(bytes(fields, field)).map_err(|_| PROTOCOL)
}
pub fn repeated<'a>(fields: &'a [WireField<'a>], field: u32) -> impl Iterator<Item = &'a [u8]> {
    fields.iter().filter_map(move |f| match &f.value {
        WireValue::Bytes(b) if f.number == field => Some(*b),
        _ => None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Id {
    pub task: u64,
    pub file: u64,
}
impl Id {
    pub fn parse(fields: &[WireField<'_>]) -> Result<Self, u64> {
        let fields = wire::decode(bytes(fields, 1)).map_err(|_| PROTOCOL)?;
        Ok(Self {
            task: number(&fields, 1),
            file: number(&fields, 2),
        })
    }
    pub fn payload(self) -> Vec<u8> {
        let mut id = Vec::new();
        wire::put_varint(1, self.task, &mut id);
        wire::put_varint(2, self.file, &mut id);
        let mut payload = Vec::new();
        wire::put_bytes(1, &id, &mut payload);
        payload
    }
}

#[derive(Clone, Debug)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub modified: u64,
}
impl FileInfo {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::put_bytes(1, self.path.as_bytes(), &mut out);
        wire::put_varint(2, self.size, &mut out);
        wire::put_varint(3, self.modified, &mut out);
        out
    }
}

pub fn inflate(input: &[u8]) -> Result<Vec<u8>, u64> {
    if input.len() > MAX_WIRE_BYTES {
        return Err(PROTOCOL);
    }
    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(input)
        .take((MAX_LIST_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| PROTOCOL)?;
    if output.len() > MAX_LIST_BYTES {
        Err(PROTOCOL)
    } else {
        Ok(output)
    }
}

pub fn compress(input: &[u8]) -> Result<Vec<u8>, u64> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(input).map_err(|_| IO)?;
    encoder.finish().map_err(|_| IO)
}

pub fn file_list(fields: &[WireField<'_>]) -> Result<Vec<FileInfo>, u64> {
    let expanded;
    let decoded;
    let list = if wire::message(fields, 6).is_some_and(|b| !b.is_empty()) {
        expanded = inflate(bytes(fields, 6))?;
        decoded = wire::decode(&expanded).map_err(|_| PROTOCOL)?;
        &decoded
    } else {
        fields
    };
    let field = if wire::message(fields, 6).is_some_and(|b| !b.is_empty()) {
        1
    } else {
        3
    };
    let mut files = Vec::new();
    for raw in repeated(list, field) {
        if files.len() >= MAX_FILES {
            return Err(PROTOCOL);
        }
        let f = wire::decode(raw).map_err(|_| PROTOCOL)?;
        files.push(FileInfo {
            path: text(&f, 1)?.into(),
            size: number(&f, 2),
            modified: number(&f, 3),
        });
    }
    Ok(files)
}

pub fn listing(files: &[FileInfo]) -> Result<Vec<u8>, u64> {
    let mut list = Vec::new();
    for file in files {
        wire::put_bytes(1, &file.encode(), &mut list);
    }
    if list.len() > MAX_LIST_BYTES {
        return Err(PROTOCOL);
    }
    compress(&list)
}

pub fn rpc(sequence: u64, request_id: u64, response: bool, case: u32, body: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    wire::put_varint(1, request_id, &mut header);
    let mut ftp = Vec::new();
    wire::put_bytes(case, body, &mut ftp);
    let mut rpc = Vec::new();
    wire::put_bytes(1, &header, &mut rpc);
    wire::put_bytes(if response { 4 } else { 8 }, &ftp, &mut rpc);
    wire::envelope(
        sequence,
        super::super::peer::now_secs(),
        if response { 22 } else { 21 },
        &rpc,
    )
}

pub struct Rpc<'a> {
    pub response: bool,
    pub request_id: u64,
    pub case: u32,
    pub body: &'a [u8],
}
pub fn parse(data: &[u8]) -> Result<Option<Rpc<'_>>, u64> {
    if data.len() > MAX_WIRE_BYTES {
        return Err(PROTOCOL);
    }
    let message = wire::decode(data).map_err(|_| PROTOCOL)?;
    let (response, rpc) = if let Some(r) = wire::message(&message, 21) {
        (false, r)
    } else if let Some(r) = wire::message(&message, 22) {
        (true, r)
    } else {
        return Ok(None);
    };
    let fields = wire::decode(rpc).map_err(|_| PROTOCOL)?;
    let Some(ftp) = wire::message(&fields, if response { 4 } else { 8 }) else {
        return Ok(None);
    };
    let header = wire::decode(bytes(&fields, 1)).map_err(|_| PROTOCOL)?;
    let cases = wire::decode(ftp).map_err(|_| PROTOCOL)?;
    if cases.len() != 1 {
        return Err(PROTOCOL);
    }
    let field = &cases[0];
    let WireValue::Bytes(body) = field.value else {
        return Err(PROTOCOL);
    };
    Ok(Some(Rpc {
        response,
        request_id: number(&header, 1),
        case: field.number,
        body,
    }))
}
