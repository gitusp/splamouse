//! Tests with a fake controller.

use super::*;
use crate::{
    calibration::StickCalibration,
    report::{RawImu, STANDARD_LEN},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

/// A fake controller that replies to the commands and subcommands, then sends `idle`.
#[derive(Clone)]
struct Fake(Rc<RefCell<State>>);

struct State {
    /// Input reports to send before `idle`.
    inputs: VecDeque<Vec<u8>>,
    /// Sent when there's nothing in `inputs`. Reads time out if `None`.
    idle: Option<Vec<u8>>,
    /// Whether to reply to the commands and subcommands.
    reply: bool,
    outputs: Vec<Vec<u8>>,
    flash: Vec<u8>,
}

impl Fake {
    fn new() -> Self {
        let mut flash = vec![0xFF; 0x10000];
        flash[0x6020..][..24].copy_from_slice(&FACTORY_IMU);
        flash[0x603D..][..18].copy_from_slice(&FACTORY_STICKS);
        Self(Rc::new(RefCell::new(State {
            inputs: VecDeque::new(),
            idle: Some(standard(0x30)),
            reply: true,
            outputs: Vec::new(),
            flash,
        })))
    }

    fn state(&self) -> std::cell::RefMut<'_, State> {
        self.0.borrow_mut()
    }

    fn push(&self, report: Vec<u8>) {
        self.state().inputs.push_back(report);
    }

    fn take_outputs(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state().outputs)
    }

    fn link(&self, usb: bool) -> Link {
        Link::new(Box::new(self.clone()), usb).unwrap()
    }

    fn connect(&self, model: Model) -> JoyCon {
        let joycon = JoyCon::connect(Box::new(self.clone()), model, false).unwrap();
        self.take_outputs();
        joycon
    }
}

impl Device for Fake {
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        let mut state = self.state();
        let Some(report) = state.inputs.pop_front().or_else(|| state.idle.clone()) else {
            drop(state);
            thread::sleep(timeout);
            return Ok(0);
        };
        buf[..report.len()].copy_from_slice(&report);
        Ok(report.len())
    }

    fn write(&self, report: &[u8]) -> Result<usize> {
        let mut state = self.state();
        state.outputs.push(report.to_vec());
        if state.reply {
            match report[0] {
                0x80 => state.inputs.push_back(vec![0x81, report[1]]),
                0x01 => {
                    let mut reply = standard(0x21);
                    reply[13] = 0x80;
                    reply[14] = report[10];
                    if report[10] == 0x10 {
                        let address = u32::from_le_bytes(report[11..15].try_into().unwrap());
                        let size = usize::from(report[15]);
                        reply[15..20].copy_from_slice(&report[11..16]);
                        reply[20..][..size]
                            .copy_from_slice(&state.flash[address as usize..][..size]);
                    }
                    state.inputs.push_back(reply);
                }
                _ => {}
            }
        }
        Ok(report.len())
    }
}

const FACTORY_IMU: [u8; 24] = [
    // Accelerometer offset and coefficient
    0x10, 0x00, 0xF0, 0xFF, 0x20, 0x00, 0x00, 0x40, 0x00, 0x40, 0x00, 0x40, //
    // Gyroscope offset and coefficient
    0x08, 0x00, 0xF8, 0xFF, 0x00, 0x00, 0x3B, 0x34, 0x3B, 0x34, 0x3B, 0x34,
];
const FACTORY_STICKS: [u8; 18] = {
    let [a, b, c] = encode_pair([0x500, 0x600]);
    let [d, e, f] = encode_pair([0x800, 0x7F0]);
    let [g, h, i] = encode_pair([0x400, 0x300]);
    // Left: above, center, below. Right: center, below, above.
    [a, b, c, d, e, f, g, h, i, d, e, f, g, h, i, a, b, c]
};

const fn encode_pair([x, y]: [u16; 2]) -> [u8; 3] {
    [x as u8, (x >> 8) as u8 | (y << 4) as u8, (y >> 4) as u8]
}

/// A standard input report with the sticks at the center of `FACTORY_STICKS`.
fn standard(id: u8) -> Vec<u8> {
    let mut report = vec![0; STANDARD_LEN];
    report[0] = id;
    report[2] = 0x8E;
    report[6..9].copy_from_slice(&encode_pair([0x800, 0x7F0]));
    report[9..12].copy_from_slice(&encode_pair([0x800, 0x7F0]));
    report
}

fn subcommand_report(counter: u8, id: u8, args: &[u8]) -> Vec<u8> {
    let mut report = vec![0; 49];
    report[0] = 0x01;
    report[1] = counter;
    report[2..10].copy_from_slice(&[0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40]);
    report[10] = id;
    report[11..][..args.len()].copy_from_slice(args);
    report
}

#[test]
fn connects_over_bluetooth() {
    let fake = Fake::new();
    let joycon = JoyCon::connect(Box::new(fake.clone()), Model::ProController, false).unwrap();
    assert!(!joycon.is_usb());
    assert_eq!(
        fake.take_outputs(),
        [
            subcommand_report(0, 0x08, &[0x00]),
            subcommand_report(1, 0x03, &[0x30]),
            subcommand_report(2, 0x40, &[0x01]),
            subcommand_report(3, 0x10, &[0x20, 0x60, 0, 0, 0x18]),
            subcommand_report(4, 0x10, &[0x26, 0x80, 0, 0, 0x1A]),
            subcommand_report(5, 0x10, &[0x3D, 0x60, 0, 0, 0x12]),
            subcommand_report(6, 0x10, &[0x10, 0x80, 0, 0, 0x16]),
        ]
    );
}

#[test]
fn connects_over_usb() {
    let fake = Fake::new();
    let joycon = JoyCon::connect(Box::new(fake.clone()), Model::ProController, true).unwrap();
    assert!(joycon.is_usb());
    let outputs = fake.take_outputs();
    let padded = |mut report: Vec<u8>| {
        report.resize(64, 0);
        report
    };
    assert_eq!(
        outputs[..6],
        [
            padded(vec![0x80, 0x01]),
            padded(vec![0x80, 0x02]),
            padded(vec![0x80, 0x03]),
            padded(vec![0x80, 0x02]),
            padded(vec![0x80, 0x04]),
            padded(subcommand_report(0, 0x08, &[0x00])),
        ]
    );
    assert!(outputs.iter().all(|report| report.len() == 64));
}

#[test]
fn packet_counter_wraps_around() {
    let fake = Fake::new();
    let mut link = fake.link(false);
    for _ in 0..17 {
        link.subcommand(0x30, &[0]).unwrap();
    }
    let counters: Vec<u8> = fake.take_outputs().iter().map(|report| report[1]).collect();
    assert_eq!(counters, [(0..16).collect::<Vec<_>>(), vec![0]].concat());
}

#[test]
fn reads_a_report() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::ProController);

    let mut report = standard(0x30);
    report[2] = 0x60; // Medium
    report[3..6].copy_from_slice(&[0b1000_1001, 0b0001_0010, 0b0100_0010]); // Y, A, ZR, Plus, Home, Up, L
    report[6..9].copy_from_slice(&encode_pair([0xD00, 0x670])); // Left: right end, half down
    report[9..12].copy_from_slice(&encode_pair([0x600, 0xFFF])); // Right: half left, beyond the top
    for (sample, at) in [13, 25, 37].into_iter().enumerate() {
        let sample = sample as i16;
        for (axis, value) in [100 + sample, -200, 4096, 8 + 1000 * sample, -8, 0]
            .into_iter()
            .enumerate()
        {
            report[at + axis * 2..][..2].copy_from_slice(&value.to_le_bytes());
        }
    }
    fake.push(report);

    let report = joycon.read().unwrap();
    let pressed: Vec<Button> = [
        Button::Y,
        Button::X,
        Button::B,
        Button::A,
        Button::R,
        Button::ZR,
        Button::Minus,
        Button::Plus,
        Button::RStick,
        Button::LStick,
        Button::Home,
        Button::Capture,
        Button::Down,
        Button::Up,
        Button::Right,
        Button::Left,
        Button::L,
        Button::ZL,
    ]
    .into_iter()
    .filter(|&button| report.buttons.is_pressed(button))
    .collect();
    assert_eq!(
        pressed,
        [
            Button::Y,
            Button::A,
            Button::ZR,
            Button::Plus,
            Button::Home,
            Button::Up,
            Button::L
        ]
    );
    assert_eq!(report.battery, BatteryLevel::Medium);
    assert_eq!(report.left_stick, Stick { x: 1.0, y: -0.5 });
    assert_eq!(report.right_stick, Stick { x: -0.5, y: 1.0 });

    // Last sample first.
    let imu = report.imu.unwrap();
    for (imu, sample) in imu.iter().zip([2.0, 1.0, 0.0]) {
        let gyro_unit = 4000.0 * 1.147 / 65535.0;
        assert_eq!(imu.gyro.x, 1000.0 * sample * gyro_unit);
        // Negated on the Pro Controller.
        assert_eq!(imu.gyro.y, -((-8.0 + 8.0) * gyro_unit));
        assert_eq!(imu.gyro.z, -0.0);
        assert_eq!(imu.accel.x, -((100.0 + sample) * 4.0 / (16383.0 - 16.0)));
        assert_eq!(imu.accel.y, -200.0 * 4.0 / (16383.0 + 16.0));
        assert_eq!(imu.accel.z, 4096.0 * 4.0 / (16383.0 - 32.0));
    }
}

#[test]
fn uses_the_user_imu_calibration() {
    let fake = Fake::new();
    let mut user = vec![0xB2, 0xA1];
    user.extend([0; 24]);
    user[2 + 12..][..2].copy_from_slice(&100i16.to_le_bytes());
    fake.state().flash[0x8026..][..26].copy_from_slice(&user);
    let mut joycon = fake.connect(Model::RightJoyCon);

    let mut report = standard(0x30);
    report[13..15].copy_from_slice(&1000i16.to_le_bytes());
    report[19..21].copy_from_slice(&100i16.to_le_bytes());
    fake.push(report);

    let imu = joycon.read().unwrap().imu.unwrap()[2];
    assert_eq!(imu.gyro, Vec3::default());
    // Negated on the right Joy-Con.
    assert_eq!(imu.accel.x, -(1000.0 * 4.0 / 16383.0));
}

#[test]
fn uses_the_user_stick_calibration() {
    let fake = Fake::new();
    // Only for the right stick: center (0x700, 0x700), 0x100 below, 0x200 above.
    let mut user = [0xFF; 22];
    user[11..13].copy_from_slice(&[0xB2, 0xA1]);
    user[13..16].copy_from_slice(&encode_pair([0x700, 0x700]));
    user[16..19].copy_from_slice(&encode_pair([0x100, 0x100]));
    user[19..22].copy_from_slice(&encode_pair([0x200, 0x200]));
    fake.state().flash[0x8010..][..22].copy_from_slice(&user);
    let mut joycon = fake.connect(Model::ProController);

    let mut report = standard(0x30);
    report[6..9].copy_from_slice(&encode_pair([0xA80, 0x7F0]));
    report[9..12].copy_from_slice(&encode_pair([0x800, 0x680]));
    fake.push(report);
    let report = joycon.read().unwrap();
    // The factory calibration for the left stick.
    assert_eq!(report.left_stick, Stick { x: 0.5, y: 0.0 });
    assert_eq!(report.right_stick, Stick { x: 0.5, y: -0.5 });
}

#[test]
fn joy_con_has_one_stick() {
    for (model, left, right) in [
        (
            Model::LeftJoyCon,
            Stick { x: 1.0, y: 1.0 },
            Stick::default(),
        ),
        (
            Model::RightJoyCon,
            Stick::default(),
            Stick { x: 1.0, y: 1.0 },
        ),
    ] {
        let fake = Fake::new();
        let mut joycon = fake.connect(model);
        let mut report = standard(0x30);
        report[6..12].fill(0xFF);
        fake.push(report);
        let report = joycon.read().unwrap();
        assert_eq!((report.left_stick, report.right_stick), (left, right));
    }
}

#[test]
fn subcommand_reply_has_no_imu() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::LeftJoyCon);
    fake.push(standard(0x21));
    assert!(joycon.read().unwrap().imu.is_none());
}

#[test]
fn rejects_unexpected_reports() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::ProController);

    fake.push(vec![0x3F; 12]);
    let error = joycon.read().unwrap_err();
    assert_eq!(error.to_string(), "unexpected input report 0x3f");

    let mut short = standard(0x30);
    short.truncate(STANDARD_LEN - 1);
    fake.push(short);
    let error = joycon.read().unwrap_err();
    assert_eq!(
        error.to_string(),
        "input report 0x30 is too short (48 bytes)"
    );
}

#[test]
fn skips_usb_replies() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::ProController);
    fake.push(vec![0x81, 0x01]);
    let mut report = standard(0x30);
    report[3] = 0b1000;
    fake.push(report);
    assert!(joycon.read().unwrap().buttons.is_pressed(Button::A));
}

#[test]
fn waits_for_the_right_reply() {
    let fake = Fake::new();
    let mut link = fake.link(false);
    fake.state().flash[0x6000..][..2].copy_from_slice(&[0x12, 0x34]);

    // Simple HID report, reply to another subcommand, and a late reply to another SPI read.
    fake.push(vec![0x3F; 12]);
    let mut other = standard(0x21);
    other[13] = 0x80;
    other[14] = 0x30;
    fake.push(other);
    let mut late = standard(0x21);
    late[13] = 0x90;
    late[14] = 0x10;
    late[15..20].copy_from_slice(&[0x00, 0x60, 0, 0, 0x01]);
    fake.push(late);

    assert_eq!(link.read_spi::<2>(0x6000).unwrap(), [0x12, 0x34]);
}

#[test]
fn fails_without_reply() {
    let fake = Fake::new();
    let mut link = fake.link(false);
    fake.state().reply = false;
    let error = link.subcommand(0x30, &[0]).unwrap_err();
    assert_eq!(error.to_string(), "no reply to subcommand 0x30");
}

#[test]
fn fails_without_reply_or_reports() {
    let fake = Fake::new();
    let mut link = fake.link(false);
    {
        let mut state = fake.state();
        state.reply = false;
        state.idle = None;
    }
    let start = Instant::now();
    let error = link.subcommand(0x30, &[0]).unwrap_err();
    assert_eq!(error.to_string(), "no reply to subcommand 0x30");
    assert!(start.elapsed() >= Duration::from_secs(1));
}

#[test]
fn fails_when_the_reports_stop() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::ProController);
    fake.state().idle = None;
    let start = Instant::now();
    let error = joycon.read().unwrap_err();
    assert_eq!(error.to_string(), "no input report for 1s");
    assert!(start.elapsed() >= Duration::from_secs(1));
}

#[test]
fn fails_when_rejected() {
    let fake = Fake::new();
    let mut link = fake.link(false);
    fake.state().reply = false;
    let mut nack = standard(0x21);
    nack[14] = 0x30;
    fake.push(nack);
    let error = link.subcommand(0x30, &[0]).unwrap_err();
    assert_eq!(error.to_string(), "the controller rejected subcommand 0x30");
}

#[test]
fn fails_on_short_writes() {
    struct Short;
    impl Device for Short {
        fn read(&self, _: &mut [u8], _: Duration) -> Result<usize> {
            Ok(0)
        }
        fn write(&self, report: &[u8]) -> Result<usize> {
            Ok(report.len() - 1)
        }
    }
    let mut link = Link::new(Box::new(Short), false).unwrap();
    let error = link.subcommand(0x30, &[0]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "wrote 48 of the 49 bytes of an output report"
    );
}

#[test]
fn sets_player_lights() {
    let fake = Fake::new();
    let mut joycon = fake.connect(Model::ProController);
    joycon
        .set_player_lights([
            PlayerLight::On,
            PlayerLight::Off,
            PlayerLight::Blinking,
            true.into(),
        ])
        .unwrap();
    assert_eq!(
        fake.take_outputs(),
        [subcommand_report(7, 0x30, &[0b0100_1001])]
    );
}

#[test]
fn battery_levels() {
    let level = |status: u8| {
        let mut report = [0; STANDARD_LEN];
        report[0] = 0x30;
        report[2] = status;
        RawReport::parse(&report).battery
    };
    assert_eq!(level(0x00), BatteryLevel::Empty);
    assert_eq!(level(0x2F), BatteryLevel::Critical);
    assert_eq!(level(0x40), BatteryLevel::Low);
    assert_eq!(level(0x71), BatteryLevel::Medium);
    assert_eq!(level(0x90), BatteryLevel::Full);
    // Undefined
    assert_eq!(level(0xE0), BatteryLevel::Full);
}

#[test]
fn raw_imu_samples() {
    let mut report = [0; STANDARD_LEN];
    report[0] = 0x31;
    for (i, byte) in report[13..].iter_mut().enumerate() {
        *byte = i as u8;
    }
    let imu = RawReport::parse(&report).imu.unwrap();
    let i16_at = |i: u8| i16::from_le_bytes([i, i + 1]);
    assert_eq!(
        imu[1],
        RawImu {
            accel: [i16_at(12), i16_at(14), i16_at(16)],
            gyro: [i16_at(18), i16_at(20), i16_at(22)],
        }
    );
}

#[test]
fn stick_with_broken_calibration() {
    let calibration = StickCalibration::left(&[0xFF; 9]);
    assert_eq!(calibration.value([0xFFF, 0xFFF]), Stick { x: 0.0, y: 0.0 });
    assert_eq!(calibration.value([0, 0]), Stick { x: -1.0, y: -1.0 });
    let calibration = StickCalibration::right(&[0; 9]);
    assert_eq!(calibration.value([0x800, 0]), Stick { x: 0.0, y: 0.0 });
}
