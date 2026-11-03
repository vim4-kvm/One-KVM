#!/usr/bin/env python3
"""Compile the production Rust writer and independently decode its SEI payloads.

Requires rustc. Does not open capture, GPU or encoder devices. Compilation must
run in the shared heavy-lock window. Hardware precision is a separate gate.
"""
from pathlib import Path
import json
import re
import struct
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
module = root / 'src/video/codec/h265_bitstream.rs'
with tempfile.TemporaryDirectory(prefix='hdr10-bitstream-') as directory:
    temp = Path(directory)
    source, binary = temp / 'writer.rs', temp / 'writer'
    source.write_text('#[allow(dead_code)]\n#[path=' + json.dumps(str(module), ensure_ascii=False) +
                      '] mod bitstream;\n' + r'''
use bitstream::{Hdr10Metadata, with_hdr10};
use std::io::Write;
fn main() {
 let mut metadata=Hdr10Metadata {mastering:[0;24],cll:[0;4]};
 for (i,value) in [13250u16,34500,7500,3000,34000,16000,15635,16450].iter().enumerate() {
  metadata.mastering[i*2..i*2+2].copy_from_slice(&value.to_be_bytes());
 }
 metadata.mastering[16..20].copy_from_slice(&10000000u32.to_be_bytes());
 metadata.mastering[20..24].copy_from_slice(&50u32.to_be_bytes());
 metadata.cll[..2].copy_from_slice(&1000u16.to_be_bytes());
 // Zero MaxFALL remains present and must not acquire an invented default.
 let au=[0,0,0,1,38,1,0x80];
 let first=with_hdr10(&au,&metadata,false).unwrap();
 let duplicate=with_hdr10(&first,&metadata,false).unwrap();
 assert_eq!(first,duplicate);
 std::io::stdout().write_all(&duplicate).unwrap();
}
''')
    subprocess.run(['rustc', '--edition=2021', str(source), '-o', str(binary)], check=True)
    data = subprocess.check_output([str(binary)])
    messages = []
    for nal in re.split(b'\x00\x00\x00?\x01', data):
        if len(nal) < 2 or nal[0] >> 1 & 63 != 39:
            continue
        rbsp = re.sub(b'\x00\x00\x03', b'\x00\x00', nal[2:])
        position = 0
        while rbsp[position:] != b'\x80':
            kind = size = 0
            while rbsp[position] == 255:
                kind += 255
                position += 1
            kind += rbsp[position]
            position += 1
            while rbsp[position] == 255:
                size += 255
                position += 1
            size += rbsp[position]
            position += 1
            payload = rbsp[position:position + size]
            assert len(payload) == size
            messages.append((kind, payload))
            position += size
    assert [kind for kind, _ in messages] == [137, 144], messages
    assert struct.unpack('>8H2I', messages[0][1]) == (
        13250, 34500, 7500, 3000, 34000, 16000, 15635, 16450, 10000000, 50)
    assert struct.unpack('>2H', messages[1][1]) == (1000, 0)
print('Production Rust HDR10 writer: independent SEI decode, zero CLL and dedup: PASS')
