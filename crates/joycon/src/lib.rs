//! Minimal driver for the Nintendo Switch Joy-Con and Pro Controller, with only what splamouse
//! uses: buttons, sticks, motion, battery level and player lights.
//!
//! Protocol: <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering>

mod calibration;
mod link;
mod report;

pub use hidapi;
pub use report::{BatteryLevel, Button, Buttons, IMU_SAMPLE_DURATION, Imu, Report, Stick, Vec3};

use anyhow::{Context, Result};
use calibration::{ImuCalibration, SticksCalibration};
use hidapi::{BusType, DeviceInfo, HidDevice};
use link::{Device, Input, Link};
use report::RawReport;
use std::time::{Duration, Instant};

const NINTENDO_VENDOR_ID: u16 = 0x057E;
/// The controller sends 60 to 120 reports per second, so it's gone if it sends none for this long.
const REPORT_TIMEOUT: Duration = Duration::from_secs(1);

/// Subcommands. <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/bluetooth_hid_subcommands_notes.md>
mod subcommand {
    pub const SET_INPUT_REPORT_MODE: u8 = 0x03;
    pub const SET_SHIPMENT_MODE: u8 = 0x08;
    pub const SPI_READ: u8 = 0x10;
    pub const SET_PLAYER_LIGHTS: u8 = 0x30;
    pub const SET_IMU_MODE: u8 = 0x40;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    LeftJoyCon,
    RightJoyCon,
    ProController,
}

impl Model {
    /// The charging grip isn't supported.
    fn from_product_id(id: u16) -> Option<Self> {
        match id {
            0x2006 => Some(Self::LeftJoyCon),
            0x2007 => Some(Self::RightJoyCon),
            0x2009 => Some(Self::ProController),
            _ => None,
        }
    }
}

/// Whether `info` is a controller that [`JoyCon::new`] supports.
pub fn is_supported(info: &DeviceInfo) -> bool {
    info.vendor_id() == NINTENDO_VENDOR_ID && Model::from_product_id(info.product_id()).is_some()
}

/// State of one player light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerLight {
    Off,
    On,
    Blinking,
}

impl From<bool> for PlayerLight {
    fn from(on: bool) -> Self {
        if on { Self::On } else { Self::Off }
    }
}

/// A connected Joy-Con or Pro Controller.
pub struct JoyCon {
    link: Link,
    model: Model,
    sticks: SticksCalibration,
    imu: ImuCalibration,
}

impl JoyCon {
    /// Sets up the controller to send the buttons, the sticks and the motion.
    pub fn new(device: HidDevice, info: &DeviceInfo) -> Result<Self> {
        let product_id = info.product_id();
        let model = Model::from_product_id(product_id)
            .with_context(|| format!("unsupported controller (product ID {product_id:#06x})"))?;
        Self::connect(
            Box::new(device),
            model,
            matches!(info.bus_type(), BusType::Usb),
        )
    }

    fn connect(device: Box<dyn Device>, model: Model, usb: bool) -> Result<Self> {
        let mut link = Link::new(device, usb)?;
        link.subcommand(subcommand::SET_SHIPMENT_MODE, &[0])?;
        link.subcommand(subcommand::SET_INPUT_REPORT_MODE, &[report::STANDARD_FULL])?;
        // Gyroscope and accelerometer
        link.subcommand(subcommand::SET_IMU_MODE, &[1])?;
        let imu = ImuCalibration::read(&mut link)?;
        let sticks = SticksCalibration::read(&mut link, model)?;
        Ok(Self {
            link,
            model,
            sticks,
            imu,
        })
    }

    /// Whether the controller is connected over USB (instead of Bluetooth).
    pub fn is_usb(&self) -> bool {
        self.link.is_usb()
    }

    /// Sets the four player lights, from the left.
    pub fn set_player_lights(&mut self, lights: [PlayerLight; 4]) -> Result<()> {
        let bits = lights
            .iter()
            .enumerate()
            .fold(0, |bits, (i, light)| match light {
                PlayerLight::Off => bits,
                PlayerLight::On => bits | 1 << i,
                PlayerLight::Blinking => bits | 1 << (i + 4),
            });
        self.link.subcommand(subcommand::SET_PLAYER_LIGHTS, &[bits])
    }

    /// Waits for the next input report.
    pub fn read(&mut self) -> Result<Report> {
        let raw = match self.link.recv(Instant::now() + REPORT_TIMEOUT)? {
            Some(Input::Standard(report)) => RawReport::parse(&report),
            Some(Input::Other(id)) => anyhow::bail!("unexpected input report {id:#04x}"),
            None => anyhow::bail!("no input report for {REPORT_TIMEOUT:?}"),
        };
        Ok(Report {
            buttons: raw.buttons,
            left_stick: self.sticks.left.value(raw.left_stick),
            right_stick: self.sticks.right.value(raw.right_stick),
            battery: raw.battery,
            // Last sample of the report first, like the joy library did.
            imu: raw
                .imu
                .map(|[a, b, c]| [c, b, a].map(|raw| self.imu.apply(raw, self.model))),
        })
    }
}

#[cfg(test)]
mod tests;
