use anyhow::Result;
use protobuf::CodedInputStream;
use std::fs;
use std::io::Read;

fn main() -> Result<()> {
    let path =
        "/Users/rohit/.gemini/antigravity/conversations/3bcb0a06-ba1b-403b-8a12-1874e8f713ca.pb";
    let data = fs::read(path)?;
    println!("Read {} bytes", data.len());

    // 0. Hex Dump first 50 bytes
    print!("Hex: ");
    for i in 0..50.min(data.len()) {
        print!("{:02X} ", data[i]);
    }
    println!("");

    // 1. Try Zstd Decompression
    println!("\n--- Attempting Zstd Decompression ---");
    if let Ok(decompressed) = zstd::decode_all(&data[..]) {
        println!(
            "✅ Zstd Decompression Successful! Size: {}",
            decompressed.len()
        );
        try_decode_protobuf(&decompressed);
        return Ok(());
    } else {
        println!("❌ Not Zstd compressed.");
    }

    // 2. Try Gzip Decompression
    println!("\n--- Attempting Gzip Decompression ---");
    let mut gz = flate2::read::GzDecoder::new(&data[..]);
    let mut s = Vec::new();
    if gz.read_to_end(&mut s).is_ok() {
        println!("✅ Gzip Decompression Successful! Size: {}", s.len());
        try_decode_protobuf(&s);
        return Ok(());
    } else {
        println!("❌ Not Gzip compressed.");
    }

    // 3. Try Zlib Decompression
    println!("\n--- Attempting Zlib Decompression ---");
    let mut z = flate2::read::ZlibDecoder::new(&data[..]);
    let mut s = Vec::new();
    if z.read_to_end(&mut s).is_ok() {
        println!("✅ Zlib Decompression Successful! Size: {}", s.len());
        try_decode_protobuf(&s);
        return Ok(());
    } else {
        println!("❌ Not Zlib compressed.");
    }

    // 4. Try Brotli Decompression
    println!("\n--- Attempting Brotli Decompression ---");
    let mut br = brotli::Decompressor::new(&data[..], 4096);
    let mut s = Vec::new();
    if br.read_to_end(&mut s).is_ok() {
        println!("✅ Brotli Decompression Successful! Size: {}", s.len());
        try_decode_protobuf(&s);
        return Ok(());
    } else {
        println!("❌ Not Brotli compressed.");
    }

    // 5. Try Snappy Decompression
    println!("\n--- Attempting Snappy Decompression ---");
    let mut decoder = snap::read::FrameDecoder::new(&data[..]);
    let mut s = Vec::new();
    if decoder.read_to_end(&mut s).is_ok() {
        println!("✅ Snappy Decompression Successful! Size: {}", s.len());
        try_decode_protobuf(&s);
        return Ok(());
    } else {
        println!("❌ Not Snappy compressed.");
    }

    // 6. Try LZ4 Decompression
    println!("\n--- Attempting LZ4 Decompression ---");
    let mut decoder = lz4::Decoder::new(&data[..])?;
    let mut s = Vec::new();
    // LZ4 decoder might return error if not lz4
    if decoder.read_to_end(&mut s).is_ok() {
        println!("✅ LZ4 Decompression Successful! Size: {}", s.len());
        try_decode_protobuf(&s);
        return Ok(());
    } else {
        println!("❌ Not LZ4 compressed.");
    }

    // 7. Scan for Protobuf Tags
    println!("\n--- Scanning for Protobuf Tags (0x0A, 0x12, 0x08) ---");
    for i in 0..100 {
        if i >= data.len() {
            break;
        }
        let byte = data[i];
        if byte == 0x0A || byte == 0x12 || byte == 0x08 {
            println!("Found candidate tag 0x{:02X} at offset {}", byte, i);
            println!("Trying to decode from offset {}...", i);
            try_decode_protobuf(&data[i..]);
            println!("--- End of attempt at offset {} ---\n", i);
        }
    }

    Ok(())
}

fn try_decode_protobuf(data: &[u8]) {
    let mut is = CodedInputStream::from_bytes(data);

    while !is.eof().unwrap_or(true) {
        match is.read_raw_varint32() {
            Ok(tag) => {
                let field_number = tag >> 3;
                let wire_type = tag & 0x7;
                print!("Field {}: WireType {} -> ", field_number, wire_type);

                match wire_type {
                    0 => {
                        // Varint
                        if let Ok(val) = is.read_uint64() {
                            println!("Varint: {}", val);
                        } else {
                            println!("Error reading varint");
                            break;
                        }
                    }
                    1 => {
                        // 64-bit
                        if let Ok(val) = is.read_fixed64() {
                            println!("Fixed64: {}", val);
                        } else {
                            println!("Error reading fixed64");
                            break;
                        }
                    }
                    2 => {
                        // Length-delimited
                        if let Ok(bytes) = is.read_bytes() {
                            if let Ok(s) = std::str::from_utf8(&bytes) {
                                let display = if s.len() > 50 { &s[..50] } else { s };
                                println!(
                                    "String/Bytes (len={}): \"{}\"...",
                                    bytes.len(),
                                    display.replace("\n", "\\n")
                                );
                            } else {
                                println!("Bytes (len={}): [binary data]", bytes.len());
                            }
                        } else {
                            println!("Error reading bytes");
                            break;
                        }
                    }
                    5 => {
                        // 32-bit
                        if let Ok(val) = is.read_fixed32() {
                            println!("Fixed32: {}", val);
                        } else {
                            println!("Error reading fixed32");
                            break;
                        }
                    }
                    _ => {
                        println!("Unknown WireType: {}", wire_type);
                        break;
                    }
                }
            }
            Err(_) => {
                println!("EOF or Error reading tag");
                break;
            }
        }
    }
}
