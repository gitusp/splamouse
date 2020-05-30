use crate::common::*;
use cgmath::{Deg, Euler, Vector3};
use std::fmt;

pub const IMU_SAMPLE_DURATION: f64 = 0.005;
pub const IMU_SAMPLES_PER_SECOND: u32 = 200;

#[repr(packed)]
#[derive(Copy, Clone)]
pub struct Frame {
    raw_accel: [I16LE; 3],
    raw_gyro: [I16LE; 3],
}

impl Frame {
    pub fn raw_accel(&self) -> Vector3<f64> {
        vector_from_raw(self.raw_accel)
    }

    pub fn raw_gyro(&self) -> Vector3<f64> {
        vector_from_raw(self.raw_gyro)
    }

    /// Calculation from https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/imu_sensor_notes.md#accelerometer---acceleration-in-g
    pub fn accel_g(&self, offset: Vector3<f64>, sens: AccSens) -> Vector3<f64> {
        (self.raw_accel() - offset) / (u16::MAX as f64 / sens.range_g() as f64)
    }

    /// The rotation described in this frame.
    /// https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/imu_sensor_notes.md#gyroscope-calibrated---rotation-in-degreess---dps
    pub fn rotation(&self, offset: Vector3<f64>, sens: GyroSens) -> Euler<Deg<f64>> {
        let dps = (self.raw_gyro() - offset) * sens.factor() * IMU_SAMPLE_DURATION;
        // TODO: define axis and make sure it's accurate
        Euler {
            x: Deg(dps.x),
            y: Deg(dps.y),
            z: Deg(dps.z),
        }
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("imu::Frame")
            .field("accel", &self.raw_accel())
            .field("gyro", &self.raw_gyro())
            .finish()
    }
}

#[repr(packed)]
#[derive(Copy, Clone, Default)]
pub struct Sensitivity {
    pub gyro_sens: GyroSens,
    pub acc_sens: AccSens,
    pub gyro_perf_rate: GyroPerfRate,
    pub acc_anti_aliasing: AccAntiAliasing,
}

/// Sensitivity range of the gyroscope.
///
/// If using DPS2000 for example, the gyroscope can measure values of
/// up to +-2000 degree per second for a total range of 4000 DPS over
/// the 16 bit raw value.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum GyroSens {
    DPS250 = 0,
    DPS500 = 1,
    DPS1000 = 2,
    DPS2000 = 3,
}

impl GyroSens {
    pub fn range_dps(self) -> u16 {
        match self {
            GyroSens::DPS250 => 500,
            GyroSens::DPS500 => 1000,
            GyroSens::DPS1000 => 2000,
            GyroSens::DPS2000 => 4000,
        }
    }

    /// factor from raw unit to dps
    pub fn factor(self) -> f64 {
        self.range_dps() as f64 / u16::MAX as f64
    }
}

impl Default for GyroSens {
    fn default() -> Self {
        GyroSens::DPS2000
    }
}

/// Sensitivity range of the accelerometer.
///
/// If using G4 for example, the accelerometer can measure values of
/// up to +-4G for a total range of 8G over the 16 bit raw value.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum AccSens {
    G8 = 0,
    G4 = 1,
    G2 = 2,
    G16 = 3,
}

impl AccSens {
    pub fn range_g(self) -> u16 {
        match self {
            AccSens::G8 => 16,
            AccSens::G4 => 8,
            AccSens::G2 => 4,
            AccSens::G16 => 32,
        }
    }
}

impl Default for AccSens {
    fn default() -> Self {
        AccSens::G8
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum GyroPerfRate {
    Hz833 = 0,
    Hz208 = 1,
}

impl Default for GyroPerfRate {
    fn default() -> Self {
        GyroPerfRate::Hz208
    }
}

/// Anti-aliasing setting of the accelerometer.
///
/// Accelerations frequencies above the value are ignored using a low-pass filter.
///
/// See https://blog.endaq.com/filter-selection-for-shock-and-vibration-applications.
#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum AccAntiAliasing {
    Hz200 = 0,
    Hz100 = 1,
}

impl Default for AccAntiAliasing {
    fn default() -> Self {
        AccAntiAliasing::Hz100
    }
}
