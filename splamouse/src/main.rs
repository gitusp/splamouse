use anyhow::{Context, Result};
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
use std::sync::{Mutex, Arc};
use std::{
    time::Duration,
    thread,
};

fn main() -> Result<()> {
    let mut api = HidApi::new()?;
    loop {
        api.refresh_devices()?;
        if let Some(device_info) = api
            .device_list()
            .find(|x| x.vendor_id() == NINTENDO_VENDOR_ID && HID_IDS.contains(&x.product_id()))
        {
            let device = device_info
                .open_device(&api)
                .with_context(|| format!("error opening the HID device {:?}", device_info))?;

            // NOTE: 接続が中途半端な際、ここでよくパニックする。
            let _ = std::panic::catch_unwind(|| -> Result<()> {
                let joycon = JoyCon::new(device, device_info.clone())?;
                hid_main(joycon).context("error running the command")?;
                Ok(())
            });
        } else {
            eprintln!("No device found");
            thread::sleep(Duration::from_secs(1));
        }
    }
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

    thread::scope(|s| {
        // ジャイロの値
        let gy = Arc::new(Mutex::new(0.0));
        let gz = Arc::new(Mutex::new(0.0));
        let _gy = Arc::clone(&gy);
        let _gz = Arc::clone(&gz);

        // スティックの値
        let sx = Arc::new(Mutex::new(0.0));
        let sy = Arc::new(Mutex::new(0.0));
        let _sx = Arc::clone(&sx);
        let _sy = Arc::clone(&sy);

        // Rボタン
        let r = Arc::new(Mutex::new(false));
        let _r = Arc::clone(&r);

        // コントローラーの姿勢
        let rot = Arc::new(Mutex::new(0.0));
        let _rot = Arc::clone(&rot);

        // 割り込みシグナル
        let interrupt = Arc::new(Mutex::new(false));
        let _interrupt = Arc::clone(&interrupt);

        // 状態取得スレッド(コントローラーの状況によって固まる)
        let handler = s.spawn(move || -> Result<()> {
            let mut enigo = Enigo::new();

            // ボタンの状態
            let mut a = false;
            let mut x = false;
            let mut y = false;
            let mut zr = false;

            loop {
                let report = joycon.tick()?;

                // ジャイロの値
                for frame in &report.imu.unwrap() {
                    let mut gy = _gy.lock().unwrap();
                    *gy = frame.gyro.y;

                    let mut gz = _gz.lock().unwrap();
                    *gz = frame.gyro.z;

                    // コントローラーの姿勢(加速度センサでドリフト修正)
                    let arot = (frame.accel[1] / frame.accel[2]).atan().to_degrees();
                    let mut rot = _rot.lock().unwrap();
                    *rot = (*rot - frame.gyro.x * 0.005) * 0.95 + arot * 0.05;
                }

                // スティックの値
                let mut sx = _sx.lock().unwrap();
                *sx = report.right_stick.x;

                let mut sy = _sy.lock().unwrap();
                *sy = report.right_stick.y;

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
                let mut r = _r.lock().unwrap();
                *r = report.buttons.right.r();

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
            }
        });

        // UI更新スレッド(リフレッシュレートより速い)
        s.spawn(move || {
            let mut enigo = Enigo::new();

            // 速度
            let mut vx = 0.0;
            let mut vy = 0.0;

            // 端数
            let mut fx = 0.0;
            let mut fy = 0.0;

            let mut last_r = false;

            loop {
                // 割り込み
                if *_interrupt.lock().unwrap() {
                    break;
                }

                // Rの押下状態(切替時は速度をリセット)
                let ur = *r.lock().unwrap();
                if last_r != ur {
                    vx = 0.0;
                    vy = 0.0;
                    fx = 0.0;
                    fy = 0.0;
                    last_r = ur;
                }

                // 速度の調整(ドリフト防止のため微量のスティックは無視)
                let usx = *sx.lock().unwrap();
                let usy = *sy.lock().unwrap();
                vx = (vx + (if usx.abs() < 0.1 { 0.0 } else { usx }) * 2.0) * 0.9;
                vy = (vy - (if usy.abs() < 0.1 { 0.0 } else { usy }) * 2.0) * 0.9;

                // モーション分
                let radians = rot.lock().unwrap().to_radians();
                let ugz = *gz.lock().unwrap();
                let ugy = *gy.lock().unwrap();
                let cos = radians.cos();
                let sin = radians.sin();
                let mx = (ugz * cos + ugy * sin) / 8.0;
                let my = (ugz * sin - ugy * cos) / 8.0;

                // 最終的に動かす量(ドリフト防止のため微量のモーションは無視)
                let dx = (vx + (if mx.abs() < 0.1 { 0.0 } else { mx })) / (if ur { 12.0 } else { 1.0 }) + fx;
                let dy = (vy + (if my.abs() < 0.1 { 0.0 } else { my })) / (if ur { 12.0 } else { 1.0 }) + fy;

                // 端数を持ち越し
                let rdx = dx.round();
                let rdy = dy.round();
                fx = dx - rdx;
                fy = dy - rdy;

                if ur {
                    // R押下中はホイール扱い
                    enigo.mouse_scroll_x(rdx as i32);
                    enigo.mouse_scroll_y(rdy as i32);
                } else {
                    // マウス移動
                    enigo.mouse_move_relative(rdx as i32, rdy as i32);
                }

                // 5ms毎に実行
                thread::sleep(Duration::from_millis(5));
            }
        });

        // センサースレッドが終了(切断等)したら、UIスレッドも落とす。
        let _ = handler.join().unwrap();
        let mut _interrupt = interrupt.lock().unwrap();
        *_interrupt = true;
    });

    Ok(())
}
