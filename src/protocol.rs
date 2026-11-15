use core::fmt::Write;
use embassy_stm32::can::Frame;
use embedded_can::Id;
use heapless::String;

use crate::{
    swd::Swd,
    text_protocol::{decimal_len, decode_hex, standard_id},
};

pub(crate) type Reply = String<192>;

pub enum Command {
    Ping,
    SwdID,
    Tx(Frame),
    Rx,
    Stream(bool),
    Stats,
}

pub fn fixed(text: &str) -> Reply {
    Reply::try_from(text).expect("reply capacity")
}

pub fn parse(text: &str) -> Result<Command, &'static str> {
    let mut words = text.split_whitespace();
    match (words.next(), words.next()) {
        (Some("PING"), None) => Ok(Command::Ping),
        (Some("SWD"), Some("ID")) if words.next().is_none() => Ok(Command::SwdID),
        (Some("CAN"), Some("RX")) if words.next().is_none() => Ok(Command::Rx),
        (Some("CAN"), Some("STATS")) if words.next().is_none() => Ok(Command::Stats),
        (Some("CAN"), Some("STREAM")) => match (words.next(), words.next()) {
            (Some("ON"), None) => Ok(Command::Stream(true)),
            (Some("OFF"), None) => Ok(Command::Stream(false)),
            _ => Err("Err SYNTAX\n"),
        },
        (Some("CAN"), Some("TX")) => {
            let (Some(id), Some(arg), data, None) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return Err("Err FRAME\n");
            };
            let (expected, hex) = match data {
                Some(hex) => (Some(decimal_len(arg).ok_or("Err FRAME\n")?), hex),
                None => (None, arg),
            };
            let id = standard_id(id).ok_or("Err FRAME\n")?;
            let (bytes, len) = decode_hex::<8>(hex, expected).ok_or("Err FRAME\n")?;
            let frame = Frame::new_standard(id, &bytes[..len]).map_err(|_| "Err FRAME\n")?;
            Ok(Command::Tx(frame))
        }
        _ => Err("Err SYNTAX\n"),
    }
}

pub fn swd_reply(swd: &mut Option<Swd>) -> Reply {
    let Some(swd) = swd else {
        return fixed("Err SWD not configured\n");
    };
    match swd.read_dpidr() {
        Some(id) => {
            let mut out = Reply::new();
            writeln!(out, "Ok DPIDR {id:08X}").unwrap();
            out
        }
        None => fixed("Err SWD\n"),
    }
}

pub fn frame_reply(frame: &Frame, stream: bool) -> Reply {
    let Id::Standard(id) = *frame.id() else {
        return fixed("Err unsupported FRAME\n");
    };
    let mut out = Reply::new();
    if stream {
        write!(out, "CAN FRAME {:03X} {} ", id.as_raw(), frame.data().len()).unwrap();
    } else {
        write!(out, "Ok RX {:03X} ", id.as_raw()).unwrap();
    }
    if frame.data().is_empty() {
        out.push('-').unwrap();
    }
    for byte in frame.data() {
        write!(out, "{byte:02X}").unwrap();
    }
    out.push('\n').unwrap();
    out
}
