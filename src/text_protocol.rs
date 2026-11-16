// 文本解析工具，用来处理 USB 分包、连续多条命令和参数边界

// 一整行接收完之后才报告错误，避免错误行的尾部被当作下一条命令执行
pub enum LineError {
    // 非换行字节数超过固定容量
    TooLong,
    // 字节不能构成有效 UTF-8
    Encoding,
}

// N 是行容量
pub struct Line<const N: usize> {
    bytes: [u8; N],
    // 已存入的有效字节数
    len: usize,
    // 超长后进入丢弃状态
    dropping: bool,
}

impl<const N: usize> Line<N> {
    // 每个新 CDC 会话使用一个新对象
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            dropping: false,
        }
    }

    // None-还未遇到换行 Some(Ok)-完整有效行 Some(Err)-完整但无效的行。
    pub fn push(&mut self, byte: u8) -> Option<Result<&str, LineError>> {
        if byte == b'\n' {
            // take 同时取出旧值并复位
            let n = core::mem::take(&mut self.len);
            if core::mem::take(&mut self.dropping) {
                return Some(Err(LineError::TooLong));
            }
            return Some(core::str::from_utf8(&self.bytes[..n]).map_err(|_| LineError::Encoding));
        }
        // \r 也作为行内容保存，CRLF 的最后一个 \n 触发解析，再由 split_whitespace 去掉 \r
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

// 把连续十六进制字符转换为字节数组
// 返回数组和实际长度
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
    // 前面已保证只有 ASCII 十六进制字符且个数为偶数，下面每两字符切片不会切断 UTF-8。
    let mut data = [0; N];
    for (i, byte) in data[..len].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some((data, len))
}
