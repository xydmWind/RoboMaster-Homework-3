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

/*
pub struct Line {
    bytes: [u8; 96],
    len: usize,
}

impl Line {
    pub const fn new() -> Self {
        Self {
            bytes: [0; 96],
            len: 0,
        }
    }

    pub fn push(&mut self, byte: u8) -> Option<&str> {
        if byte == b'\n' {
            let n = core::mem::take(&mut self.len);
            // A full buffer may contain a truncated command. Discard that line.
            if n == self.bytes.len() {
                return Some("");
            }
            return Some(core::str::from_utf8(&self.bytes[..n]).unwrap_or_default());
        }
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
        None
    }
}

fn fixed(text: &str) -> Reply {
    Reply::try_from(text).expect("fixed reply fits one short USB packet")
}

pub async fn execute(command: &str, can: &mut Can<'_>, swd: &mut Option<Swd>) -> Reply {
    let mut words = command.split_ascii_whitespace();
    let mut out = Reply::new();
    match (words.next(), words.next()) {
        (Some("PING"), None) => return fixed("OK DM-MC02\n"),
        (Some("SWD"), Some("ID")) if words.next().is_none() => {
            let Some(swd) = swd.as_mut() else {
                return fixed("ERR SWD_NOT_CONFIGURED\n");
            };
            match swd.read_dpidr() {
                Some(id) => writeln!(out, "OK DPIDR {id:08X}").expect("SWD reply fits"),
                None => return fixed("ERR SWD\n"),
            }
        }
        (Some("CAN"), Some("TX")) => {
            let (Some(id), Some(hex), None) = (words.next(), words.next(), words.next()) else {
                return fixed("ERR SYNTAX\n");
            };
            let Some(frame) = parse_frame(id, hex) else {
                return fixed("ERR FRAME\n");
            };
            return match with_timeout(Duration::from_millis(100), can.write(&frame)).await {
                Ok(None) => fixed("OK QUEUED\n"),
                _ => fixed("ERR CAN TX\n"),
            };
        }
        (Some("CAN"), Some("RX")) if words.next().is_none() => {
            let envelope = match with_timeout(Duration::from_millis(100), can.read()).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(_)) => return fixed("ERR CAN RX\n"),
                Err(_) => return fixed("EMPTY\n"),
            };
            let frame = envelope.frame;
            let Id::Standard(id) = *frame.id() else {
                return fixed("ERR UNSUPPORTED_FRAME\n");
            };
            if frame.is_remote_frame() {
                return fixed("ERR UNSUPPORTED_FRAME\n");
            }
            write!(out, "OK RX {:03X} ", id.as_raw()).expect("CAN prefix fits");
            if frame.data().is_empty() {
                out.push('-').expect("empty payload fits");
            }
            for byte in frame.data() {
                write!(out, "{byte:02X}").expect("8-byte payload fits");
            }
            out.push('\n').expect("line ending fits");
        }
        _ => return fixed("ERR SYNTAX\n"),
    }
    out
}

fn parse_frame(id: &str, payload: &str) -> Option<Frame> {
    let id = u16::from_str_radix(id, 16).ok()?;
    let mut bytes = [0; 8];
    let payload = if payload == "-" { "" } else { payload };
    if !payload.is_ascii() || payload.len() > 16 || !payload.len().is_multiple_of(2) {
        return None;
    }
    for (i, byte) in bytes[..payload.len() / 2].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&payload[i * 2..i * 2 + 2], 16).ok()?;
    }
    Frame::new_standard(id, &bytes[..payload.len() / 2]).ok()
}
 */
