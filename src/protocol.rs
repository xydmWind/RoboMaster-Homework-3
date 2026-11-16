// 把完整文本行转换为 Command，并把结果转换为主机可读的回复

use core::fmt::Write;
use embassy_stm32::can::Frame;
use embedded_can::Id;
use heapless::String;

use crate::{
    swd::Swd,
    text_protocol::{decimal_len, decode_hex, standard_id},
};

// 192 字节回复
pub(crate) type Reply = String<192>;

// 解析成功后的指令
pub enum Command {
    Ping,
    SwdID,
    Tx(Frame),
    Rx,
    Stream(bool),
    Stats,
}

// 将常量回复装入固定容量字符串
pub fn fixed(text: &str) -> Reply {
    Reply::try_from(text).expect("reply capacity")
}

// 只检查格式并构造指令
// 出现 ERR SYNTAX 时检查关键字/多余参数，ERR FRAME 则检查 ID、长度和十六进制数据。
pub fn parse(text: &str) -> Result<Command, &'static str> {
    // 允许空格、制表符以及 CRLF 中残留的 \r
    let mut words = text.split_whitespace();
    match (words.next(), words.next()) {
        (Some("PING"), None) => Ok(Command::Ping),
        (Some("SWD"), Some("ID")) if words.next().is_none() => Ok(Command::SwdID),
        (Some("CAN"), Some("RX")) if words.next().is_none() => Ok(Command::Rx),
        (Some("CAN"), Some("STATS")) if words.next().is_none() => Ok(Command::Stats),
        (Some("CAN"), Some("STREAM")) => match (words.next(), words.next()) {
            (Some("ON"), None) => Ok(Command::Stream(true)),
            (Some("OFF"), None) => Ok(Command::Stream(false)),
            _ => Err("ERR SYNTAX\n"),
        },
        (Some("CAN"), Some("TX")) => {
            // 新格式的长度为十进制，ID 为不带 0x 的十六进制
            let (Some(id), Some(arg), data, None) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return Err("ERR FRAME\n");
            };
            // 有第三个字段时 arg 是声明长度，只有两个字段时 arg 本身就是载荷
            let (expected, hex) = match data {
                Some(hex) => (Some(decimal_len(arg).ok_or("ERR FRAME\n")?), hex),
                None => (None, arg),
            };
            // 先校验再构造帧
            // 在任一步失败时立刻返回错误
            let id = standard_id(id).ok_or("ERR FRAME\n")?;
            let (bytes, len) = decode_hex::<8>(hex, expected).ok_or("ERR FRAME\n")?;
            let frame = Frame::new_standard(id, &bytes[..len]).map_err(|_| "ERR FRAME\n")?;
            Ok(Command::Tx(frame))
        }
        _ => Err("ERR SYNTAX\n"),
    }
}

// SWD_NOT_CONFIGURED 表示 wiring 尚未返回 Some，ERR SWD 表示读取失败
pub fn swd_reply(swd: &mut Option<Swd>) -> Reply {
    let Some(swd) = swd else {
        return fixed("ERR SWD_NOT_CONFIGURED\n");
    };
    match swd.read_dpidr() {
        Some(id) => {
            let mut out = Reply::new();
            writeln!(out, "OK DPIDR {id:08X}").unwrap();
            out
        }
        None => fixed("ERR SWD\n"),
    }
}

pub fn frame_reply(frame: &Frame, stream: bool) -> Reply {
    let Id::Standard(id) = *frame.id() else {
        return fixed("ERR UNSUPPORTED_FRAME\n");
    };
    let mut out = Reply::new();
    if stream {
        write!(out, "CAN FRAME {:03X} {} ", id.as_raw(), frame.data().len()).unwrap();
    } else {
        write!(out, "OK RX {:03X} ", id.as_raw()).unwrap();
    }
    // 使用 '-' 表示零字节
    if frame.data().is_empty() {
        out.push('-').unwrap();
    }
    for byte in frame.data() {
        write!(out, "{byte:02X}").unwrap();
    }
    out.push('\n').unwrap();
    out
}
