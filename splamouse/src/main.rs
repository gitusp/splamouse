use anyhow::{Context, Result};
use cgmath::{Vector3};
use enigo::*;
use joycon::{
    hidapi::HidApi,
    joycon_sys::{
        input::{BatteryLevel },
        light::{self, PlayerLight},
        HID_IDS, NINTENDO_VENDOR_ID,
    },
    JoyCon,
};
use std::{
    time::Duration,
};
use std::{time::Instant};
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

fn main() -> Result<()> {
    let formatter = tracing_subscriber::fmt()
        .with_span_events(if std::env::var("LOG_TIMING").is_ok() {
            FmtSpan::CLOSE
        } else {
            FmtSpan::NONE
        })
        .with_env_filter(EnvFilter::from_default_env());
    if std::env::var("LOG_PRETTY").is_ok() {
        formatter.pretty().init();
    } else {
        formatter.init();
    }

    let api = HidApi::new()?;
    loop {
        if let Some(device_info) = api
            .device_list()
            .find(|x| x.vendor_id() == NINTENDO_VENDOR_ID && HID_IDS.contains(&x.product_id()))
        {
            let device = device_info
                .open_device(&api)
                .with_context(|| format!("error opening the HID device {:?}", device_info))?;

            let joycon = JoyCon::new(device, device_info.clone())?;

            hid_main(joycon).context("error running the command")?;

            break;
        } else {
            eprintln!("No device found");
            break;
        }
    }
    Ok(())
}

fn hid_main(mut joycon: JoyCon) -> Result<()> {
    joycon.set_home_light(light::HomeLight::new(
        0x8,
        0x2,
        0x0,
        &[(0xf, 0xf, 0), (0x2, 0xf, 0)],
    ))?;

    let battery_level = joycon.tick()?.info.battery_level();

    joycon.set_player_light(light::PlayerLights::new(
        (battery_level >= BatteryLevel::Full).into(),
        (battery_level >= BatteryLevel::Medium).into(),
        (battery_level >= BatteryLevel::Low).into(),
        if battery_level >= BatteryLevel::Low {
            PlayerLight::On
        } else {
            PlayerLight::Blinking
        },
    ))?;

    monitor(&mut joycon)?;
    Ok(())
}

fn monitor(joycon: &mut JoyCon) -> Result<()> {
    joycon.enable_imu()?;
    joycon.load_calibration()?;

    let mut now = Instant::now();
    let mut enigo = Enigo::new();

    let mut vx = 0.0;
    let mut vy = 0.0;

    let mut a = false;
    let mut x = false;
    let mut y = false;
    let mut r = false;
    let mut zr = false;

    loop {
        let report = joycon.tick()?;
        let mut last_rot = Vector3::unit_x();

        for frame in &report.imu.unwrap() {
            last_rot = frame.gyro;
        }

        // Aボタン押下時
        if report.buttons.right.a() {
            if !a {
                enigo.key_down(Key::Meta);
                enigo.key_click(Key::RightArrow);
                enigo.key_up(Key::Meta);
                a = true;
            }
        } else {
            if a {
                a = false;
            }
        }

        // Xボタン押下時
        if report.buttons.right.x() {
            if !x {
                enigo.key_down(Key::Control);
                x = true;
            }
        } else {
            if x {
                enigo.key_up(Key::Control);
                x = false;
            }
        }

        // Yボタン押下時
        if report.buttons.right.y() {
            if !y {
                enigo.key_down(Key::Meta);
                enigo.key_click(Key::LeftArrow);
                enigo.key_up(Key::Meta);
                y = true;
            }
        } else {
            if y {
                y = false;
            }
        }

        // Rボタン押下時
        if report.buttons.right.r() {
            if !r {
                vx = 0.0;
                vy = 0.0;
                r = true;
            }
        } else {
            if r {
                vx = 0.0;
                vy = 0.0;
                r = false;
            }
        }

        // ZRボタン押下時
        if report.buttons.right.zr() {
            if !zr {
                enigo.mouse_down(MouseButton::Left);
                zr = true;
            }
        } else {
            if zr {
                enigo.mouse_up(MouseButton::Left);
                zr = false;
            }
        }

        if now.elapsed() > Duration::from_millis(15) {
            now = Instant::now();

            // 微量のスティックは無視(ドリフト防止)
            let x = if report.right_stick.x.abs() < 0.1 { 0.0 } else { report.right_stick.x };
            let y = if report.right_stick.y.abs() < 0.1 { 0.0 } else { report.right_stick.y };

            // 加速度の調整
            vx = (vx + x * 16.0) * 0.8;
            vy = (vy - y * 16.0) * 0.8;

            if r {
                // R押下中はホイール扱い
                enigo.mouse_scroll_x(((vx + last_rot.z / 8.0) / 8.0) as i32);
                enigo.mouse_scroll_y(((vy - last_rot.y / 8.0) / 8.0) as i32);
            } else {
                // マウス移動
                enigo.mouse_move_relative((vx + last_rot.z / 4.0) as i32, (vy - last_rot.y / 4.0) as i32);
            }
        }
    }
}
