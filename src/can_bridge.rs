// 通过控制 CDC 实现电脑与 CAN1 的持续双向桥接。

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

// CAN 接收长期运行
pub async fn run(usb: CdcAcmClass<'static, Usb>, can: Can<'static>, mut swd: Option<Swd>) {
    // 拆分读写端，避免一个任务等待接收时长期独占对象
    let (mut can_tx, can_rx, _properties) = can.split();
    let (mut usb_tx, mut usb_rx, control) = usb.split_with_control();
    let sessions = async {
        loop {
            wait_open(&mut usb_rx, &control).await;
            clear_session();
            let generation = GENERATION.fetch_add(1, Relaxed).wrapping_add(1);
            ACTIVE.store(true, Relaxed);
            // select3 使三条 I/O 流程并发推进
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
    // 接收任务不随串口开关而停止
    // 无活动会话的帧会被主动丢弃并计数
    join(sessions, can_rx_task(can_rx)).await;
}

// 清理本层的会话状态，恢复为 STREAM OFF
fn clear_session() {
    STREAM.store(false, Relaxed);
    while TX.try_receive().is_ok() {}
    while REPLIES.try_receive().is_ok() {}
    // 清掉尚未交给电脑的接收帧，也计入 rx_drop
    while RX.try_receive().is_ok() {
        RX_DROP.fetch_add(1, Relaxed);
    }
    WAKE_WRITER.reset();
}

// USB 接口启用后，还要等待主机
async fn wait_open(usb: &mut Receiver<'static, Usb>, control: &ControlChanged<'static>) {
    let mut discard = [0; PACKET];
    'connection: loop {
        usb.wait_connection().await;
        while !control.dtr() {
            // 等待期间读到的数据直接丢弃
            if let Either::Second(Err(_)) =
                select(control.control_changed(), usb.read_packet(&mut discard)).await
            {
                continue 'connection;
            }
        }
        return;
    }
}

// 等控制线状态变化
async fn wait_close(control: &ControlChanged<'static>) {
    while control.dtr() {
        control.control_changed().await;
    }
}

// 只负责排队并通知写任务
// 回复队列满时暂停
async fn reply(value: Reply) {
    REPLIES.send(value).await;
    WAKE_WRITER.signal(());
}

// 拼接完整行 -> 解析 -> 执行轻量控制或把 CAN 帧交给发送队列
async fn host_rx(usb: &mut Receiver<'static, Usb>, swd: &mut Option<Swd>) {
    let mut packet = [0; PACKET];
    // 包缓冲是 64 字节，行缓冲是 96 字节
    let mut line = Line::<96>::new();
    while let Ok(n) = usb.read_packet(&mut packet).await {
        for &byte in &packet[..n] {
            let Some(command) = line.push(byte) else {
                continue;
            };
            let command = match command {
                Ok(text) => protocol::parse(text),
                Err(LineError::TooLong) => Err("ERR LINE_TOO_LONG\n"),
                Err(LineError::Encoding) => Err("ERR SYNTAX\n"),
            };
            let response = match command {
                Err(error) => fixed(error),
                Ok(Command::Ping) => fixed("OK DM-MC02\n"),
                Ok(Command::SwdID) => protocol::swd_reply(swd),
                Ok(Command::Tx(frame)) => {
                    // 这里只进入软件队列，成功后 continue
                    if TX.try_send(frame).is_ok() {
                        continue;
                    }
                    TX_BUSY.fetch_add(1, Relaxed);
                    fixed("ERR BUSY\n")
                }
                Ok(Command::Stream(enabled)) => {
                    STREAM.store(enabled, Relaxed);
                    fixed(if enabled {
                        "OK STREAM ON\n"
                    } else {
                        "OK STREAM OFF\n"
                    })
                }
                Ok(Command::Rx) => {
                    // 自动上报和手动取帧共用 RX 队列
                    if STREAM.load(Relaxed) {
                        fixed("ERR STREAM_ACTIVE\n")
                    } else {
                        // 先取已经排队的帧
                        // 没有时最多等 100 ms，再回 EMPTY
                        match with_timeout(Duration::from_millis(100), RX.receive()).await {
                            Ok((_, frame)) => protocol::frame_reply(&frame, false),
                            Err(_) => fixed("EMPTY\n"),
                        }
                    }
                }
                Ok(Command::Stats) => {
                    let mut out = Reply::new();
                    writeln!(
                        out,
                        "OK STATS tx={} rx={} rx_drop={} tx_busy={} tx_timeout={} rx_error={}",
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

// 从软件 TX 队列逐帧交给 FDCAN
// 发送回复可能晚于之后收到的 PING
async fn can_tx_task(can: &mut CanTx<'static>) {
    loop {
        let frame = TX.receive().await;
        let response = match with_timeout(Duration::from_millis(100), can.write(&frame)).await {
            Ok(None) => {
                TX_COUNT.fetch_add(1, Relaxed);
                fixed("OK QUEUED\n")
            }
            _ => {
                // 超时或意外出现替换帧返回值都进入此分支
                TX_TIMEOUT.fetch_add(1, Relaxed);
                fixed("ERR CAN_TX_TIMEOUT\n")
            }
        };
        reply(response).await;
    }
}

// 持续接收与 USB 发送分离
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
                // 错误可能连续出现，稍等 1 ms
                Timer::after_millis(1).await;
            }
        }
        yield_now().await;
    }
}

// 把回复和主动上报按完整行串行输出。
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
        // 空闲时等待通知
        if !sent {
            WAKE_WRITER.wait().await;
        }
    }
}

// 192 字节回复可能跨多个 USB 包，每块最多 63 字节，使每个包都短于 64 字节最大包长
async fn write_line(usb: &mut Sender<'static, Usb>, line: &Reply) -> bool {
    for chunk in line.as_bytes().chunks(PACKET - 1) {
        if usb.write_packet(chunk).await.is_err() {
            return false;
        }
    }
    true
}
