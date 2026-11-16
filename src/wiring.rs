// 基础作业使用 P16

use crate::swd::Swd;
use embassy_stm32::{
    Peripherals, bind_interrupts,
    can::Can,
    gpio::{Flex, Level, Output, Speed},
    peripherals,
    usart::{self, BufferedUart, Config, DataBits, Parity, StopBits},
};
use static_cell::StaticCell;

// 将 UART8 的硬件中断交给 BufferedUart
bind_interrupts!(struct UartIrqs {
    UART7 => usart::BufferedInterruptHandler<peripherals::UART7>;
});

pub struct Ports {
    pub can: Can<'static>,
    pub uart: Option<BufferedUart<'static>>,
}

// 构造供 main 使用的 USB、CAN、UART 和软件 SWD
pub fn configure(p: Peripherals) -> (crate::board::Usb, Ports, Option<Swd>) {
    let usb = crate::board::usb(p.USB_OTG_HS, p.PA12, p.PA11);

    // SWCLK 始终由本板输出，所以使用初始为低的 Output
    // SWDIO 要在发送请求与读取目标回复之间切换方向，所以使用 Flex
    // Speed::Low 控制 GPIO 速度
    let swd = Some(Swd::new(
        Output::new(p.PE9, Level::Low, Speed::Low),
        Flex::new(p.PE13),
    ));

    // 驱动在 configure 返回之后仍会使用缓冲区，因此必须放在静态存储区
    // 这两个 512 字节用来缓冲短时速率差
    static TX_BUF: StaticCell<[u8; 512]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 512]> = StaticCell::new();

    // 115200 baud、8 个数据位、无校验、1 个停止位
    let mut config = Config::default();
    config.baudrate = 115200;
    config.data_bits = DataBits::DataBits8;
    config.parity = Parity::ParityNone;
    config.stop_bits = StopBits::STOP1;

    // Embassy STM32 参数顺序是外设、RX、TX、TX 缓冲、RX 缓冲、中断、配置
    // 先传 RX=PE0，再传 TX=PE1
    // unwrap 遇到错误会触发 panic
    let uart = Some(
        BufferedUart::new(
            p.UART7,
            p.PE7,
            p.PE8,
            TX_BUF.init([0; 512]),
            RX_BUF.init([0; 512]),
            UartIrqs,
            config,
        )
        .unwrap(),
    );

    // CAN1 继续使用模板的板载收发器与 PD0/RX、PD1/TX
    let can = crate::board::can1(p.FDCAN1, p.PD0, p.PD1);
    (usb, Ports { can, uart }, swd)
}
