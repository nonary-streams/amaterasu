//! Bounded line framing shared by worker pipes and development console input.
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};

const MAX_LINE_BYTES: usize = 65_536;

pub fn read_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return String::from_utf8(line)
                .map(Some)
                .map_err(std::io::Error::other);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if line.len() + consumed > MAX_LINE_BYTES {
            return Err(std::io::Error::other("control line exceeds 64 KiB"));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            line.pop();
            return String::from_utf8(line)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    }
}

pub fn lines(input: impl Read + Send + 'static) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::sync_channel(8);
    // A blocked console reader cannot block shutdown. Worker processes are reaped;
    // console input threads are reclaimed when the command-line process exits.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(input);
        loop {
            match read_line(&mut reader) {
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_final_line_and_rejects_oversized_or_invalid_utf8() {
        let mut input = &b"first\nlast"[..];
        assert_eq!(read_line(&mut input).unwrap().as_deref(), Some("first"));
        assert_eq!(read_line(&mut input).unwrap().as_deref(), Some("last"));
        assert!(read_line(&mut input).unwrap().is_none());
        assert!(read_line(&mut vec![b'x'; MAX_LINE_BYTES + 1].as_slice()).is_err());
        assert!(read_line(&mut &b"\xff\n"[..]).is_err());
    }
}
