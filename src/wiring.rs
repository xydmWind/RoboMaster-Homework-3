use crate::swd::Swd;
use embassy_stm32::{
    Peripherals, bind_interrupts,
    can::Can,
    gpio::{Flex, Level, Output, Speed},
    peripherals,
    usart::{self, BufferedUart, Config, DataBits, Parity, StopBits},
};
use static_cell::StaticCell;

bind_interrupts!(struct UartIrqs {
    UART8 => usart::BufferedInterruptHandler<peripherals::UART8>;
});

pub struct Ports {
    pub can: Can<'static>,
    pub uart: Option<BufferedUart<'static>>,
}

pub fn configure(p: Peripherals) -> (crate::board::Usb, Ports, Option<Swd>) {
    let usb = crate::board::usb(p.USB_OTG_HS, p.PA12, p.PA11);

    // SWD GPIO
    let swd = Some(Swd::new(
        Output::new(p.PD14, Level::Low, Speed::Low),
        Flex::new(p.PD15),
    ));

    // UART
    static TX_BUF: StaticCell<[u8; 512]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 512]> = StaticCell::new();

    let mut config = Config::default();
    config.baudrate = 115200;
    config.data_bits = DataBits::DataBits8;
    config.parity = Parity::ParityNone;
    config.stop_bits = StopBits::STOP1;

    let uart = Some(
        BufferedUart::new(
            p.UART8,
            p.PE0,
            p.PE1,
            TX_BUF.init([0; 512]),
            RX_BUF.init([0; 512]),
            UartIrqs,
            config,
        )
        .unwrap(),
    );

    let can = crate::board::can1(p.FDCAN1, p.PD0, p.PD1);
    (usb, Ports { can, uart }, swd)
}
