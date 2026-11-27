//! Calibration of the sticks and the IMU, read from the SPI flash.
//!
//! <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/spi_flash_notes.md>

use crate::{
    Model,
    link::Link,
    report::{Imu, RawImu, Stick, decode_pair},
};
use anyhow::Result;

const FACTORY_IMU: u32 = 0x6020;
const FACTORY_STICKS: u32 = 0x603D;
const USER_STICKS: u32 = 0x8010;
const USER_IMU: u32 = 0x8026;
/// Each user calibration, set from the console, starts with this magic number if there is one.
const USER_MAGIC: [u8; 2] = [0xB2, 0xA1];

/// Rotation speed of one unit of the raw gyroscope value, at the default ±2000 dps range.
const GYRO_DPS_PER_UNIT: f64 = 4000.0 * 1.147 / 65535.0;

pub(crate) struct SticksCalibration {
    pub left: StickCalibration,
    pub right: StickCalibration,
}

impl SticksCalibration {
    /// Uses the user calibration of each stick if there is one, and the factory calibration
    /// otherwise. The stick a Joy-Con doesn't have stays at the center.
    pub(crate) fn read(link: &mut Link, model: Model) -> Result<Self> {
        let factory: [u8; 18] = link.read_spi(FACTORY_STICKS)?;
        let user: [u8; 22] = link.read_spi(USER_STICKS)?;
        let (factory_left, factory_right) = factory.split_at(9);
        let (user_left, user_right) = user.split_at(11);
        Ok(Self {
            left: match model {
                Model::RightJoyCon => StickCalibration::NONE,
                _ => StickCalibration::left(user_or(user_left, factory_left)),
            },
            right: match model {
                Model::LeftJoyCon => StickCalibration::NONE,
                _ => StickCalibration::right(user_or(user_right, factory_right)),
            },
        })
    }
}

/// The user calibration if it has been set, and `factory` otherwise.
fn user_or<'a>(user: &'a [u8], factory: &'a [u8]) -> &'a [u8] {
    match user.split_first_chunk() {
        Some((&USER_MAGIC, user)) => user,
        _ => factory,
    }
}

/// Range of the raw values of a stick, for each axis.
#[derive(Debug, PartialEq)]
pub(crate) struct StickCalibration {
    min: [f64; 2],
    center: [f64; 2],
    max: [f64; 2],
}

impl StickCalibration {
    /// Always at the center.
    const NONE: Self = Self {
        min: [0.0; 2],
        center: [0.0; 2],
        max: [0.0; 2],
    };

    /// The left stick's calibration has, in this order, how far the stick goes above the center,
    /// the center, and how far it goes below the center.
    pub(crate) fn left(data: &[u8]) -> Self {
        Self::new(
            decode_pair(&data[3..]),
            decode_pair(&data[..3]),
            decode_pair(&data[6..]),
        )
    }

    /// The right stick's calibration has the center first, then how far the stick goes below and
    /// above it.
    pub(crate) fn right(data: &[u8]) -> Self {
        Self::new(
            decode_pair(&data[..3]),
            decode_pair(&data[6..]),
            decode_pair(&data[3..]),
        )
    }

    fn new(center: [u16; 2], above: [u16; 2], below: [u16; 2]) -> Self {
        Self {
            min: [0, 1].map(|i| center[i].saturating_sub(below[i]).into()),
            center: center.map(f64::from),
            max: [0, 1].map(|i| (center[i] + above[i]).min(0xFFF).into()),
        }
    }

    pub(crate) fn value(&self, raw: [u16; 2]) -> Stick {
        let [x, y] = [0, 1].map(|i| {
            let (min, center, max) = (self.min[i], self.center[i], self.max[i]);
            let offset = f64::from(raw[i]).max(min).min(max) - center;
            let range = if offset >= 0.0 {
                max - center
            } else {
                center - min
            };
            // With a broken calibration, the range can be 0 (and then the offset too).
            if range == 0.0 { 0.0 } else { offset / range }
        });
        Stick { x, y }
    }
}

pub(crate) struct ImuCalibration {
    accel_offset: [f64; 3],
    gyro_offset: [f64; 3],
}

impl ImuCalibration {
    /// Uses the user calibration if there is one, and the factory calibration otherwise.
    pub(crate) fn read(link: &mut Link) -> Result<Self> {
        let factory: [u8; 24] = link.read_spi(FACTORY_IMU)?;
        let user: [u8; 26] = link.read_spi(USER_IMU)?;
        Ok(Self::new(user_or(&user, &factory)))
    }

    /// `data` has, as 3 values (x, y, z) each, the accelerometer offset and coefficient, then the
    /// gyroscope offset and coefficient.
    fn new(data: &[u8]) -> Self {
        let xyz = |at: usize| {
            [0, 1, 2].map(|axis| {
                let i = at + axis * 2;
                f64::from(i16::from_le_bytes([data[i], data[i + 1]]))
            })
        };
        Self {
            accel_offset: xyz(0),
            gyro_offset: xyz(12),
        }
    }

    pub(crate) fn apply(&self, raw: RawImu, model: Model) -> Imu {
        let mut gyro: crate::Vec3 = [0, 1, 2]
            .map(|i| (f64::from(raw.gyro[i]) - self.gyro_offset[i]) * GYRO_DPS_PER_UNIT)
            .into();
        let mut accel: crate::Vec3 = [0, 1, 2]
            .map(|i| f64::from(raw.accel[i]) * 4.0 / (16383.0 - self.accel_offset[i]))
            .into();
        // The controllers don't have the same axes.
        match model {
            Model::LeftJoyCon | Model::ProController => {
                gyro.y = -gyro.y;
                gyro.z = -gyro.z;
                accel.x = -accel.x;
            }
            Model::RightJoyCon => {
                accel.x = -accel.x;
                accel.y = -accel.y;
                accel.z = -accel.z;
            }
        }
        Imu { gyro, accel }
    }
}
