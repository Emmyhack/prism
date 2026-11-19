//! Minimal deterministic ICNS writer for release packaging.

use std::{env, fs, io, path::PathBuf};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let output = PathBuf::from(
        arguments
            .next()
            .ok_or_else(|| invalid("usage: prism-icns-packager OUTPUT TYPE=PNG ..."))?,
    );
    let mut chunks = Vec::new();
    for argument in arguments {
        let argument = argument
            .into_string()
            .map_err(|_| invalid("icon arguments must be valid UTF-8"))?;
        let (kind, path) = argument
            .split_once('=')
            .ok_or_else(|| invalid("icon argument must use TYPE=PNG"))?;
        let kind: [u8; 4] = kind
            .as_bytes()
            .try_into()
            .map_err(|_| invalid("ICNS chunk type must contain exactly four bytes"))?;
        let png = fs::read(path)?;
        if !png.starts_with(PNG_SIGNATURE) {
            return Err(invalid(format!("{path} is not a PNG file")).into());
        }
        let length = u32::try_from(png.len().saturating_add(8))
            .map_err(|_| invalid("PNG is too large for an ICNS chunk"))?;
        chunks.push((kind, length, png));
    }
    if chunks.is_empty() {
        return Err(invalid("at least one icon chunk is required").into());
    }
    let total = chunks.iter().try_fold(8_u32, |total, (_, length, _)| {
        total
            .checked_add(*length)
            .ok_or_else(|| invalid("ICNS file is too large"))
    })?;
    let mut encoded = Vec::with_capacity(total as usize);
    encoded.extend_from_slice(b"icns");
    encoded.extend_from_slice(&total.to_be_bytes());
    for (kind, length, png) in chunks {
        encoded.extend_from_slice(&kind);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&png);
    }
    fs::write(output, encoded)?;
    Ok(())
}
