use crate::{
    PACKET,
    board::Usb,
    protocol::{self, Command, Reply, fixed},
    swd::Swd,
    text_protocol::{Line, LineError},
};
use core::{
    fmt::Write,
    sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
};
use embassy_futures::{
    join::join,
    select::{Either, select, select3},
    yield_now,
};
use embassy_stm32::can::{Can, CanRx, CanTx, Frame};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Timer, with_timeout};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender};
use embedded_can::Id;

const TX_CAPACITY: usize = 16;
const RX_CAPACITY: usize = 32;
const REPLY_CAPACITY: usize = 8;
static TX: Channel<CriticalSectionRawMutex, Frame, TX_CAPACITY> = Channel::new();
static RX: Channel<CriticalSectionRawMutex, (u32, Frame), RX_CAPACITY> = Channel::new();
static REPLIES: Channel<CriticalSectionRawMutex, Reply, REPLY_CAPACITY> = Channel::new();
static WAKE_WRITER: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static STREAM: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU32 = AtomicU32::new(0);
static TX_COUNT: AtomicU32 = AtomicU32::new(0);
static RX_COUNT: AtomicU32 = AtomicU32::new(0);
static RX_DROP: AtomicU32 = AtomicU32::new(0);
static TX_BUSY: AtomicU32 = AtomicU32::new(0);
static TX_TIMEOUT: AtomicU32 = AtomicU32::new(0);
static RX_ERROR: AtomicU32 = AtomicU32::new(0);

pub async fn run(usb: CdcAcmClass<'static, Usb>, can: Can<'static>, mut swd: Option<Swd>) {
    let (mut can_tx, can_rx, _properties) = can.split();
    let (mut usb_tx, mut usb_rx, control) = usb.split_with_control();
    let sessions = async {
        loop {
            wait_open(&mut usb_rx, &control).await;
            clear_session();
            let generation = GENERATION.fetch_add(1, Relaxed).wrapping_add(1);
            ACTIVE.store(true, Relaxed);
            select(
                select3(
                    host_rx(&mut usb_rx, &mut swd),
                    can_tx_task(&mut can_tx),
                    usb_tx_task(&mut usb_tx, generation),
                ),
                wait_close(&control),
            )
            .await;
            ACTIVE.store(false, Relaxed);
            clear_session();
        }
    };
    join(sessions, can_rx_task(can_rx)).await;
}

fn clear_session() {
    STREAM.store(false, Relaxed);
    while TX.try_receive().is_ok() {}
    while REPLIES.try_receive().is_ok() {}
    while RX.try_receive().is_ok() {
        RX_DROP.fetch_add(1, Relaxed);
    }
    WAKE_WRITER.reset();
}

async fn wait_open(usb: &mut Receiver<'static, Usb>, control: &ControlChanged<'static>) {
    let mut discard = [0; PACKET];
    'connection: loop {
        usb.wait_connection().await;
        while !control.dtr() {
            if let Either::Second(Err(_)) =
                select(control.control_changed(), usb.read_packet(&mut discard)).await
            {
                continue 'connection;
            }
        }
        return;
    }
}

async fn wait_close(control: &ControlChanged<'static>) {
    while control.dtr() {
        control.control_changed().await;
    }
}

async fn reply(value: Reply) {
    REPLIES.send(value).await;
    WAKE_WRITER.signal(());
}

async fn host_rx(usb: &mut Receiver<'static, Usb>, swd: &mut Option<Swd>) {
    let mut packet = [0; PACKET];
    let mut line = Line::<96>::new();
    while let Ok(n) = usb.read_packet(&mut packet).await {
        for &byte in &packet[..n] {
            let Some(command) = line.push(byte) else {
                continue;
            };
            let command = match command {
                Ok(text) => protocol::parse(text),
                Err(LineError::TooLong) => Err("Err ling too long\n"),
                Err(LineError::Encoding) => Err("Err SYNTAX\n"),
            };
            let response = match command {
                Err(error) => fixed(error),
                Ok(Command::Ping) => fixed("Ok DM-MC02\n"),
                Ok(Command::SwdID) => protocol::swd_reply(swd),
                Ok(Command::Tx(frame)) => {
                    if TX.try_send(frame).is_ok() {
                        continue;
                    }
                    TX_BUSY.fetch_add(1, Relaxed);
                    fixed("Err busy\n")
                }
                Ok(Command::Stream(enabled)) => {
                    STREAM.store(enabled, Relaxed);
                    fixed(if enabled {
                        "Ok STREAM ON\n"
                    } else {
                        "Ok STREAM OFF\n"
                    })
                }
                Ok(Command::Rx) => {
                    if STREAM.load(Relaxed) {
                        fixed("Err STREAM active\n")
                    } else {
                        match with_timeout(Duration::from_millis(100), RX.receive()).await {
                            Ok((_, frame)) => protocol::frame_reply(&frame, false),
                            Err(_) => fixed("empty\n"),
                        }
                    }
                }
                Ok(Command::Stats) => {
                    let mut out = Reply::new();
                    writeln!(
                        out,
                        "Ok STATS tx={} rx={} rx_drop={} tx_busy={} tx_timeout={} rx_error={}",
                        TX_COUNT.load(Relaxed),
                        RX_COUNT.load(Relaxed),
                        RX_DROP.load(Relaxed),
                        TX_BUSY.load(Relaxed),
                        TX_TIMEOUT.load(Relaxed),
                        RX_ERROR.load(Relaxed)
                    )
                    .unwrap();
                    out
                }
            };
            reply(response).await;
        }
    }
}

async fn can_tx_task(can: &mut CanTx<'static>) {
    loop {
        let frame = TX.receive().await;
        let response = match with_timeout(Duration::from_millis(100), can.write(&frame)).await {
            Ok(None) => {
                TX_COUNT.fetch_add(1, Relaxed);
                fixed("Ok queued\n")
            }
            _ => {
                TX_TIMEOUT.fetch_add(1, Relaxed);
                fixed("Err CAN TX timeout\n")
            }
        };
        reply(response).await;
    }
}

async fn can_rx_task(mut can: CanRx<'static>) {
    loop {
        match can.read().await {
            Ok(envelope) => {
                RX_COUNT.fetch_add(1, Relaxed);
                let frame = envelope.frame;
                let valid = matches!(frame.id(), Id::Standard(_))
                    && !frame.header().rtr()
                    && !frame.header().fdcan();
                if !valid
                    || !ACTIVE.load(Relaxed)
                    || RX.try_send((GENERATION.load(Relaxed), frame)).is_err()
                {
                    RX_DROP.fetch_add(1, Relaxed);
                } else {
                    WAKE_WRITER.signal(());
                }
            }
            Err(_) => {
                RX_ERROR.fetch_add(1, Relaxed);
                Timer::after_millis(1).await;
            }
        }
        yield_now().await;
    }
}

async fn usb_tx_task(usb: &mut Sender<'static, Usb>, generation: u32) {
    loop {
        let mut sent = false;
        for _ in 0..4 {
            let Ok(response) = REPLIES.try_receive() else {
                break;
            };
            if !write_line(usb, &response).await {
                return;
            }
            sent = true;
        }
        if STREAM.load(Relaxed)
            && let Ok((session, frame)) = RX.try_receive()
        {
            if session == generation && !write_line(usb, &protocol::frame_reply(&frame, true)).await
            {
                return;
            }
            sent = true;
        }
        if !sent {
            WAKE_WRITER.wait().await;
        }
    }
}

async fn write_line(usb: &mut Sender<'static, Usb>, line: &Reply) -> bool {
    for chunk in line.as_bytes().chunks(PACKET - 1) {
        if usb.write_packet(chunk).await.is_err() {
            return false;
        }
    }
    true
}
