#![no_std]

// USB interface configuration & setup. This will create two endpoints. One is
// an application endpoint which contains the USB serial which we use for 
// application communications and the second is the log output endpoint.
//
// See https://github.com/embassy-rs/embassy/blob/main/examples/rp/src/bin/usb_serial_with_logger.rs
// for the basis of this file.

use embassy_futures::join::join4;
use embassy_rp::{bind_interrupts, Peri};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::InterruptHandler;
use embassy_usb::{
    Builder, Config,
    class::cdc_acm::{CdcAcmClass, State}
};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_io_async::Write;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

const BUF_SIZE_DESCRIPTOR: usize = 256;
const BUF_SIZE_CONTROL: usize = 64;
const MAX_PACKET_SIZE: usize = 64;

#[embassy_executor::task]
pub async fn usb_handler(usb: Peri<'static, USB>,
                         serial: &'static str,
                         mut rx_pipe: embassy_sync::pipe::Writer<'static, CriticalSectionRawMutex, 128>,
                         tx_pipe: embassy_sync::pipe::Reader<'static, CriticalSectionRawMutex, 128>)
{
    // Create the driver, from the HAL.
    let driver = embassy_rp::usb::Driver::new(usb, Irqs);

    // Create embassy-usb Config
    let config = {
        let mut config = Config::new(0x1781, 0x0e6c);
        config.manufacturer = Some("Embassy");
        config.product = Some("USB-serial example");
        config.serial_number = Some(serial);
        config.max_power = 100;
        config.max_packet_size_0 = 64;
        // TODO: Update The Serial Number From JDEC or something similar

        // Required for windows compatibility.
        // https://developer.nordicsemi.com/nRF_Connect_SDK/doc/1.9.1/kconfig/CONFIG_CDC_ACM_IAD.html#help
        //config.device_class = USB_CLASS_CDC;
        //config.device_sub_class = USB_CDC_SUBCLASS_ACM;
        //config.device_protocol = USB_CDC_PROTOCOL_AT;
        config
    };

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; BUF_SIZE_DESCRIPTOR];
    let mut bos_descriptor = [0; BUF_SIZE_DESCRIPTOR];
    let mut msos_descriptor = [0; BUF_SIZE_DESCRIPTOR];
    let mut control_buf = [0; BUF_SIZE_CONTROL];

    let mut state = State::new();
    let mut logger_state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );

    // Create The Serial Class for the CLI. 
    let serial = CdcAcmClass::new(&mut builder, &mut state, MAX_PACKET_SIZE as u16);

    // Create a class for the logger
    let logger_class = CdcAcmClass::new(&mut builder, &mut logger_state, MAX_PACKET_SIZE as u16);

    // Creates the logger and returns the logger future
    // Note: You'll need to use log::info! afterwards instead of info! for this to work (this also applies to all the other log::* macros)
    let log_fut = embassy_usb_logger::with_class!(1024, log::LevelFilter::Info, logger_class);
    
    // Set Up Handling for Serial
    let (mut send, mut recv) = serial.split();        
    
    // Reader function, pull packets from the interface as they come in and publish them into the rx pub/sub queue
    let usb_reader_fut = async move {
        recv.wait_connection().await;
        loop {
            let mut buf: [u8; MAX_PACKET_SIZE] = [0; MAX_PACKET_SIZE];
            let len = recv.read_packet(&mut buf).await.unwrap();
            let _ = rx_pipe.write_all(&buf[0..len]).await;
        }
    };

    // Writer function, pull blocks of up to MAX_PACKET_SIZE from the tx pub/sub queue and push out to the
    // the USB interface.
    let usb_writer_fut = async {
        send.wait_connection().await;
        loop {
            let mut buf: [u8; MAX_PACKET_SIZE] = [0; MAX_PACKET_SIZE];
            let len = tx_pipe.read(&mut buf).await;
            send.write_packet(&mut buf[0..len]).await.unwrap();
        }
    };

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    join4(usb_fut, log_fut, usb_reader_fut, usb_writer_fut).await;
}