//! Sending reports to the controller and reading its input reports.
//!
//! <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/bluetooth_hid_notes.md>

use crate::{
    report::{
        STANDARD_FULL, STANDARD_FULL_MCU, STANDARD_LEN, SUBCOMMAND_ACK, SUBCOMMAND_DATA,
        SUBCOMMAND_ID, SUBCOMMAND_REPLY,
    },
    subcommand,
};
use anyhow::{Result, bail, ensure};
use hidapi::HidDevice;
use std::time::{Duration, Instant};

/// Longest input report (0x31, with the MCU data).
const MAX_INPUT_LEN: usize = 362;
/// Output report with rumble data and a subcommand.
const RUMBLE_AND_SUBCOMMAND: u8 = 0x01;
const SUBCOMMAND_REPORT_LEN: usize = 49;
const SUBCOMMAND_ARGS: usize = 11;
/// Rumble data that doesn't vibrate (320 Hz and 160 Hz at amplitude 0, for both sides).
const NO_RUMBLE: [u8; 8] = [0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40];
/// The reply to a subcommand comes within this many input reports.
const SUBCOMMAND_REPLY_REPORTS: u32 = 10;

/// Every output report has to be padded to this size over USB.
const USB_PACKET_LEN: usize = 64;
/// Output report ID of the USB-only proprietary commands (`80 xx`).
const USB_COMMAND_ID: u8 = 0x80;
/// Input report ID of the replies to the USB-only commands (`81 xx`).
const USB_REPLY_ID: u8 = 0x81;
const USB_REPLY_TIMEOUT: Duration = Duration::from_millis(100);
const USB_COMMAND_RETRIES: u32 = 5;

/// USB-only proprietary commands.
///
/// <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/USB-HID-Notes.md>
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum UsbCommand {
    Status = 0x01,
    Handshake = 0x02,
    HighSpeed = 0x03,
    /// Only talk over USB, without timing out and reverting to Bluetooth.
    ForceUsb = 0x04,
}

/// The HID device of the controller. Tests use a fake controller instead.
pub(crate) trait Device {
    /// Reads an input report into `buf` and returns its length, or 0 if none came within
    /// `timeout`. Waits forever if `timeout` is `None`.
    fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> Result<usize>;
    /// Writes an output report and returns how many bytes were written.
    fn write(&self, report: &[u8]) -> Result<usize>;
}

impl Device for HidDevice {
    fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> Result<usize> {
        let timeout = timeout.map_or(-1, |timeout| {
            i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX)
        });
        Ok(self.read_timeout(buf, timeout)?)
    }

    fn write(&self, report: &[u8]) -> Result<usize> {
        Ok(HidDevice::write(self, report)?)
    }
}

/// An input report.
pub(crate) enum Input {
    /// A standard input report (0x21, 0x30 or 0x31).
    Standard([u8; STANDARD_LEN]),
    /// Another input report, with its ID.
    Other(u8),
}

pub(crate) struct Link {
    device: Box<dyn Device>,
    usb: bool,
    /// Packet counter of the output reports with a subcommand.
    counter: u8,
}

impl Link {
    pub(crate) fn new(device: Box<dyn Device>, usb: bool) -> Result<Self> {
        let mut link = Self {
            device,
            usb,
            counter: 0,
        };
        if usb {
            link.setup_usb()?;
        }
        Ok(link)
    }

    pub(crate) fn is_usb(&self) -> bool {
        self.usb
    }

    /// Over USB, the controller only sends regular reports after this
    /// handshake, and reverts to Bluetooth unless forced to stay on USB.
    /// Same sequence as SDL's HIDAPI Switch driver.
    fn setup_usb(&mut self) -> Result<()> {
        ensure!(
            self.usb_command(UsbCommand::Status, true)?,
            "no reply to the USB status command"
        );
        // Clones may not reply to these, so like SDL we don't fail on them.
        self.usb_command(UsbCommand::Handshake, true)?;
        self.usb_command(UsbCommand::HighSpeed, true)?;
        // The baud rate switch only takes effect after a second handshake.
        self.usb_command(UsbCommand::Handshake, true)?;
        ensure!(
            self.usb_command(UsbCommand::ForceUsb, false)?,
            "failed to force USB mode"
        );
        Ok(())
    }

    /// Returns whether the command was written (and acknowledged, if `wait_reply`).
    fn usb_command(&mut self, command: UsbCommand, wait_reply: bool) -> Result<bool> {
        let mut report = [0; USB_PACKET_LEN];
        report[0] = USB_COMMAND_ID;
        report[1] = command as u8;
        for _ in 0..USB_COMMAND_RETRIES {
            if self.device.write(&report).is_err() {
                continue;
            }
            if !wait_reply || self.wait_usb_reply(command)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn wait_usb_reply(&mut self, command: UsbCommand) -> Result<bool> {
        let deadline = Instant::now() + USB_REPLY_TIMEOUT;
        let mut buf = [0; 512];
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let len = self.device.read(&mut buf, Some(remaining))?;
            if len >= 2 && buf[0] == USB_REPLY_ID && buf[1] == command as u8 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn send(&mut self, report: &[u8]) -> Result<()> {
        let mut padded = [0; USB_PACKET_LEN];
        let report = if self.usb && report.len() < USB_PACKET_LEN {
            padded[..report.len()].copy_from_slice(report);
            &padded[..]
        } else {
            report
        };
        let written = self.device.write(report)?;
        ensure!(
            written == report.len(),
            "wrote {written} of the {} bytes of an output report",
            report.len()
        );
        Ok(())
    }

    /// Waits for the next input report.
    pub(crate) fn recv(&mut self) -> Result<Input> {
        let mut buf = [0; MAX_INPUT_LEN];
        loop {
            let len = self.device.read(&mut buf, None)?;
            // Late replies to the USB-only commands aren't regular input reports.
            if len == 0 || buf[0] == USB_REPLY_ID {
                continue;
            }
            let id = buf[0];
            if !matches!(id, SUBCOMMAND_REPLY | STANDARD_FULL | STANDARD_FULL_MCU) {
                return Ok(Input::Other(id));
            }
            ensure!(
                len >= STANDARD_LEN,
                "input report {id:#04x} is too short ({len} bytes)"
            );
            let mut report = [0; STANDARD_LEN];
            report.copy_from_slice(&buf[..STANDARD_LEN]);
            return Ok(Input::Standard(report));
        }
    }

    /// Sends a subcommand and waits for the controller to accept it.
    pub(crate) fn subcommand(&mut self, id: u8, args: &[u8]) -> Result<()> {
        self.subcommand_with_reply(id, args, |_| true)?;
        Ok(())
    }

    /// Reads `N` bytes of the SPI flash from `address`.
    pub(crate) fn read_spi<const N: usize>(&mut self, address: u32) -> Result<[u8; N]> {
        const { assert!(N <= 0x1D, "the controller reads at most 0x1D bytes at once") };
        let [a, b, c, d] = address.to_le_bytes();
        let args = [a, b, c, d, N as u8];
        // The reply has the address and the size, then the data. Check them in case a reply to
        // another read arrives late.
        let reply = self.subcommand_with_reply(subcommand::SPI_READ, &args, |reply| {
            reply.starts_with(&args)
        })?;
        let mut data = [0; N];
        data.copy_from_slice(&reply[SUBCOMMAND_DATA + args.len()..][..N]);
        Ok(data)
    }

    /// Sends a subcommand and waits for its reply, which `is_reply` checks with the reply data.
    /// Returns the input report with the reply.
    fn subcommand_with_reply(
        &mut self,
        id: u8,
        args: &[u8],
        is_reply: impl Fn(&[u8]) -> bool,
    ) -> Result<[u8; STANDARD_LEN]> {
        let mut report = [0; SUBCOMMAND_REPORT_LEN];
        report[0] = RUMBLE_AND_SUBCOMMAND;
        report[1] = self.counter;
        report[2..10].copy_from_slice(&NO_RUMBLE);
        report[10] = id;
        report[SUBCOMMAND_ARGS..][..args.len()].copy_from_slice(args);
        self.counter = (self.counter + 1) & 0xF;
        self.send(&report)?;

        for _ in 0..SUBCOMMAND_REPLY_REPORTS {
            if let Input::Standard(input) = self.recv()?
                && input[0] == SUBCOMMAND_REPLY
                && input[SUBCOMMAND_ID] == id
                && is_reply(&input[SUBCOMMAND_DATA..])
            {
                ensure!(
                    input[SUBCOMMAND_ACK] & 0x80 != 0,
                    "the controller rejected subcommand {id:#04x}"
                );
                return Ok(input);
            }
        }
        bail!("no reply to subcommand {id:#04x}")
    }
}
