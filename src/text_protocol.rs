pub enum LineError {
    TooLong,
    Encoding,
}

pub struct Line<const N: usize> {
    bytes: [u8; N],
    len: usize,
    dropping: bool,
}

impl<const N: usize> Line<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            dropping: false,
        }
    }

    pub fn push(&mut self, byte: u8) -> Option<Result<&str, LineError>> {
        if byte == b'\n' {
            let n = core::mem::take(&mut self.len);
            if core::mem::take(&mut self.dropping) {
                return Some(Err(LineError::TooLong));
            }
            return Some(core::str::from_utf8(&self.bytes[..n]).map_err(|_| LineError::Encoding));
        }
        if !self.dropping {
            if self.len == N {
                self.dropping = true;
            } else {
                self.bytes[self.len] = byte;
                self.len += 1;
            }
        }
        None
    }
}

pub fn standard_id(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(text, 16).ok().filter(|id| *id <= 0x7ff)
}

pub fn decimal_len(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub fn decode_hex<const N: usize>(text: &str, expected: Option<usize>) -> Option<([u8; N], usize)> {
    let hex = if text == "-" { "" } else { text };
    if (hex.is_empty() && text != "-")
        || hex.len() > N * 2
        || !hex.len().is_multiple_of(2)
        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let len = hex.len() / 2;
    if expected.is_some_and(|expected| expected != len) {
        return None;
    }
    let mut data = [0; N];
    for (i, byte) in data[..len].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some((data, len))
}
