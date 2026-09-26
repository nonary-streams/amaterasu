use crate::PackageResult;
use sha2::{Digest, Sha256};
use std::io::Read;

pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn decode_hex<const N: usize>(value: &str) -> PackageResult<[u8; N]> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid hexadecimal encoding".into());
    }

    let mut decoded = [0; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let pair = &value[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16).map_err(|error| error.to_string())?;
    }
    Ok(decoded)
}

pub fn sha256(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

pub fn sha256_reader(mut reader: impl Read) -> PackageResult<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let size = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(encode_hex(&digest.finalize()))
}

pub fn random_identifier() -> PackageResult<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(encode_hex(&bytes))
}
