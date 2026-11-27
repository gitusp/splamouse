//! Standard input reports.
//!
//! <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/bluetooth_hid_notes.md#standard-input-report-format>

/// Seconds between the IMU samples, three of which come in each report.
pub const IMU_SAMPLE_DURATION: f64 = 0.005;

/// Standard input report with the reply to a subcommand.
pub(crate) const SUBCOMMAND_REPLY: u8 = 0x21;
/// Standard input report with the IMU samples.
pub(crate) const STANDARD_FULL: u8 = 0x30;
/// Standard input report with the IMU samples and MCU data.
pub(crate) const STANDARD_FULL_MCU: u8 = 0x31;
/// Length of a standard input report, up to the end of the IMU samples or the subcommand reply.
pub(crate) const STANDARD_LEN: usize = 49;

// Offsets in a standard input report.
const STATUS: usize = 2;
const BUTTONS: usize = 3;
const LEFT_STICK: usize = 6;
const RIGHT_STICK: usize = 9;
const IMU: usize = 13;
pub(crate) const SUBCOMMAND_ACK: usize = 13;
pub(crate) const SUBCOMMAND_ID: usize = 14;
pub(crate) const SUBCOMMAND_DATA: usize = 15;

/// State of the controller in one input report.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub buttons: Buttons,
    pub left_stick: Stick,
    pub right_stick: Stick,
    pub battery: BatteryLevel,
    /// The three IMU samples of the report. `None` in a report without them (a subcommand reply).
    pub imu: Option<[Imu; 3]>,
}

/// Position of a stick, from -1.0 to 1.0. Right and up are positive.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stick {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl From<[f64; 3]> for Vec3 {
    fn from([x, y, z]: [f64; 3]) -> Self {
        Self { x, y, z }
    }
}

/// One sample of the IMU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Imu {
    /// Rotation speed, in degrees per second.
    pub gyro: Vec3,
    /// Acceleration, in G.
    pub accel: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BatteryLevel {
    Empty,
    Critical,
    Low,
    Medium,
    Full,
}

impl BatteryLevel {
    /// From the device status byte, whose high 3 bits are the level.
    fn from_status(status: u8) -> Self {
        match status >> 5 {
            0 => Self::Empty,
            1 => Self::Critical,
            2 => Self::Low,
            3 => Self::Medium,
            // 5 to 7 aren't defined.
            _ => Self::Full,
        }
    }
}

/// Buttons of the Pro Controller layout, with their bit in [`Buttons`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Y = 0,
    X = 1,
    B = 2,
    A = 3,
    R = 6,
    ZR = 7,
    Minus = 8,
    Plus = 9,
    RStick = 10,
    LStick = 11,
    Home = 12,
    Capture = 13,
    Down = 16,
    Up = 17,
    Right = 18,
    Left = 19,
    L = 22,
    ZL = 23,
}

/// The buttons pressed in a report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons(u32);

impl Buttons {
    pub fn is_pressed(self, button: Button) -> bool {
        (self.0 >> button as u32) & 1 != 0
    }
}

/// One IMU sample, before calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawImu {
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
}

/// The values in a standard input report, before calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawReport {
    pub buttons: Buttons,
    pub battery: BatteryLevel,
    pub left_stick: [u16; 2],
    pub right_stick: [u16; 2],
    pub imu: Option<[RawImu; 3]>,
}

impl RawReport {
    /// `report` is a standard input report (0x21, 0x30 or 0x31).
    pub(crate) fn parse(report: &[u8; STANDARD_LEN]) -> Self {
        let i16_at = |i: usize| i16::from_le_bytes([report[i], report[i + 1]]);
        let imu = matches!(report[0], STANDARD_FULL | STANDARD_FULL_MCU).then(|| {
            [0, 1, 2].map(|sample| {
                let at = IMU + sample * 12;
                RawImu {
                    accel: [0, 1, 2].map(|axis| i16_at(at + axis * 2)),
                    gyro: [0, 1, 2].map(|axis| i16_at(at + 6 + axis * 2)),
                }
            })
        });
        Self {
            buttons: Buttons(u32::from_le_bytes([
                report[BUTTONS],
                report[BUTTONS + 1],
                report[BUTTONS + 2],
                0,
            ])),
            battery: BatteryLevel::from_status(report[STATUS]),
            left_stick: decode_pair(&report[LEFT_STICK..][..3]),
            right_stick: decode_pair(&report[RIGHT_STICK..][..3]),
            imu,
        }
    }
}

/// Decodes the two 12-bit values packed in 3 bytes, as in the stick positions and calibration.
pub(crate) fn decode_pair(bytes: &[u8]) -> [u16; 2] {
    let [a, b, c] = [bytes[0], bytes[1], bytes[2]].map(u16::from);
    [a | (b & 0xF) << 8, b >> 4 | c << 4]
}
