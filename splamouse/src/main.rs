use anyhow::{Context, Result};
use clap::Parser;
use enigo::{
    Axis, Button, Coordinate,
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Mouse, NewConError, Settings,
};
use joycon::{
    hidapi::{BusType, HidApi},
    joycon_sys::{
        input::{BatteryLevel },
        light::{self, PlayerLight},
        HID_IDS, NINTENDO_VENDOR_ID,
    },
    JoyCon,
};
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, Arc};
use std::{
    time::Duration,
    thread,
};

#[derive(Parser)]
struct Opts {
    #[arg(short, long, default_value = "0.0")]
    pub gyro: f64,
    #[arg(short, long, default_value = "0.0")]
    pub stick: f64,
}

fn main() -> Result<()> {
    let opts = Opts::parse();

    if opts.gyro.abs() > 5.0 || opts.stick.abs() > 5.0 {
        eprintln!("gyro and stick must be between -5.0 and 5.0");
        return Ok(());
    }

    // 感度計算
    let gyro = 2.0 + opts.gyro * 0.2;
    let stick = 2.0 + opts.stick * 0.2;

    // ⌘+クリックや ⌘+スクロールが効くよう、ボタンとマウスの両スレッドで1つの Enigo を共有して修飾キーの状態を揃える。
    // Enigo は drop 時に送ったイベント数に比例してスリープする(5ms 毎のマウス移動だと 1 時間で約 3 時間)ので、
    // 接続ごとに作り直さず、drop しないよう 'static にして使い回す。
    let enigo: &'static Mutex<Enigo> = Box::leak(Box::new(Mutex::new(new_enigo()?)));

    let mut api = HidApi::new()?;
    loop {
        api.refresh_devices()?;
        // USB と Bluetooth の両方で見えている場合は USB を優先する。
        if let Some(device_info) = api
            .device_list()
            .filter(|x| x.vendor_id() == NINTENDO_VENDOR_ID && HID_IDS.contains(&x.product_id()))
            .min_by_key(|x| !matches!(x.bus_type(), BusType::Usb))
        {
            let device = device_info
                .open_device(&api)
                .with_context(|| format!("error opening the HID device {:?}", device_info))?;

            // NOTE: 接続が中途半端な際、ここでよくパニックする。
            // パニック後は device を捨てて再接続するだけなので AssertUnwindSafe で問題ない。
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                let joycon = JoyCon::new(device, device_info.clone())?;
                eprintln!(
                    "Connected via {}",
                    if joycon.is_usb() { "USB" } else { "Bluetooth" }
                );
                hid_main(joycon, enigo, gyro, stick).context("error running the command")?;
                Ok(())
            }));
            release_held_keys(enigo);
        } else {
            eprintln!("No device found");
            thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Enigo を作る。アクセシビリティ権限が許可されるまで待ち、許可ダイアログは初回だけ出す。
fn new_enigo() -> Result<Enigo> {
    let mut open_prompt = true;
    loop {
        let settings = Settings {
            // 旧 enigo (0.0.14) と同じく、物理キーボードと状態を共有するイベントソースを使う。
            independent_of_keyboard_state: false,
            open_prompt_to_get_permissions: open_prompt,
            ..Settings::default()
        };
        match Enigo::new(&settings) {
            Ok(enigo) => return Ok(enigo),
            Err(NewConError::NoPermission) => {
                if open_prompt {
                    eprintln!(
                        "Waiting for the accessibility permission \
                         (System Settings > Privacy & Security > Accessibility)"
                    );
                    open_prompt = false;
                }
                thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e).context("error initializing the input simulator"),
        }
    }
}

/// 押しっぱなしのキーを離す。切断時に ⌘ などが残ると、再接続後のクリック等にも付いてしまう。
fn release_held_keys(enigo: &Mutex<Enigo>) {
    let mut enigo = enigo.lock().unwrap();
    let (keys, _) = enigo.held();
    for key in keys {
        let _ = enigo.key(key, Release);
    }
}

fn hid_main(mut joycon: JoyCon, enigo: &Mutex<Enigo>, gyro: f64, stick: f64) -> Result<()> {
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

    monitor(&mut joycon, enigo, gyro, stick)?;
    Ok(())
}

fn monitor(joycon: &mut JoyCon, enigo: &Mutex<Enigo>, gyro: f64, stick: f64) -> Result<()> {
    joycon.enable_imu()?;
    joycon.load_calibration()?;

    thread::scope(|s| {
        // ジャイロの値
        let gy = Arc::new(Mutex::new(0.0));
        let gz = Arc::new(Mutex::new(0.0));
        let _gy = Arc::clone(&gy);
        let _gz = Arc::clone(&gz);

        // L-スティックの値
        let slx = Arc::new(Mutex::new(0.0));
        let sly = Arc::new(Mutex::new(0.0));
        let _slx = Arc::clone(&slx);
        let _sly = Arc::clone(&sly);

        // R-スティックの値
        let srx = Arc::new(Mutex::new(0.0));
        let sry = Arc::new(Mutex::new(0.0));
        let _srx = Arc::clone(&srx);
        let _sry = Arc::clone(&sry);

        // コントローラーの姿勢
        let rot = Arc::new(Mutex::new(0.0));
        let _rot = Arc::clone(&rot);

        // 割り込みシグナル
        let interrupt = Arc::new(Mutex::new(false));
        let _interrupt = Arc::clone(&interrupt);

        // 状態取得スレッド(コントローラーの状況によって固まる)
        let handler = s.spawn(move || -> Result<()> {
            // ボタンの状態
            let mut a = false;
            let mut b = false;
            let mut x = false;
            let mut y = false;
            let mut l = false;
            let mut r = false;
            let mut zl = false;
            let mut zr = false;
            let mut minus = false;
            let mut plus = false;
            let mut left = false;
            let mut right = false;
            let mut down = false;
            let mut up = false;
            let mut capture = false;
            let mut home = false;
            let mut lstick = false;
            let mut rstick = false;

            loop {
                let mut should_sleep = false;

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

                // L-スティックの値
                let mut slx = _slx.lock().unwrap();
                *slx = report.left_stick.x;
                let mut sly = _sly.lock().unwrap();
                *sly = report.left_stick.y;

                // R-スティックの値
                let mut srx = _srx.lock().unwrap();
                *srx = report.right_stick.x;
                let mut sry = _sry.lock().unwrap();
                *sry = report.right_stick.y;

                let mut enigo = enigo.lock().unwrap();

                // Aボタン押下時
                if report.buttons.right.a() {
                    if !a {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::RightArrow, Press)?;
                        should_sleep = true;
                        a = true;
                    }
                } else {
                    if a {
                        enigo.key(Key::RightArrow, Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        a = false;
                    }
                }

                // Bボタン押下時
                if report.buttons.right.b() {
                    if !b {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::LeftArrow, Press)?;
                        should_sleep = true;
                        b = true;
                    }
                } else {
                    if b {
                        enigo.key(Key::LeftArrow, Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        b = false;
                    }
                }

                // マイナスボタン押下時
                if report.buttons.middle.minus() {
                    if !minus {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Other(0x0D), Press)?;
                        should_sleep = true;
                        minus = true;
                    }
                } else {
                    if minus {
                        enigo.key(Key::Other(0x0D), Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        minus = false;
                    }
                }

                // プラスボタン押下時
                if report.buttons.middle.plus() {
                    if !plus {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Other(0x0F), Press)?;
                        should_sleep = true;
                        plus = true;
                    }
                } else {
                    if plus {
                        enigo.key(Key::Other(0x0F), Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        plus = false;
                    }
                }

                // 左ボタン押下時
                if report.buttons.left.left() {
                    if !left {
                        enigo.key(Key::Control, Press)?;
                        enigo.key(Key::Shift, Press)?;
                        enigo.key(Key::Tab, Press)?;
                        should_sleep = true;
                        left = true;
                    }
                } else {
                    if left {
                        enigo.key(Key::Tab, Release)?;
                        enigo.key(Key::Shift, Release)?;
                        enigo.key(Key::Control, Release)?;
                        should_sleep = true;
                        left = false;
                    }
                }

                // 右ボタン押下時
                if report.buttons.left.right() {
                    if !right {
                        enigo.key(Key::Control, Press)?;
                        enigo.key(Key::Tab, Press)?;
                        should_sleep = true;
                        right = true;
                    }
                } else {
                    if right {
                        enigo.key(Key::Tab, Release)?;
                        enigo.key(Key::Control, Release)?;
                        should_sleep = true;
                        right = false;
                    }
                }

                // 下ボタン押下時
                if report.buttons.left.down() {
                    if !down {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Other(0x08), Press)?;
                        should_sleep = true;
                        down = true;
                    }
                } else {
                    if down {
                        enigo.key(Key::Other(0x08), Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        down = false;
                    }
                }

                // 上ボタン押下時
                if report.buttons.left.up() {
                    if !up {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Other(0x09), Press)?;
                        should_sleep = true;
                        up = true;
                    }
                } else {
                    if up {
                        enigo.key(Key::Other(0x09), Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        up = false;
                    }
                }

                // ホーム押下時
                if report.buttons.middle.home() {
                    if !home {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Shift, Press)?;
                        enigo.key(Key::Other(0x06), Press)?;
                        should_sleep = true;
                        home = true;
                    }
                } else {
                    if home {
                        enigo.key(Key::Other(0x06), Release)?;
                        enigo.key(Key::Shift, Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        home = false;
                    }
                }

                // 撮影ボタン押下時
                if report.buttons.middle.capture() {
                    if !capture {
                        enigo.key(Key::Meta, Press)?;
                        enigo.key(Key::Other(0x06), Press)?;
                        should_sleep = true;
                        capture = true;
                    }
                } else {
                    if capture {
                        enigo.key(Key::Other(0x06), Release)?;
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        capture = false;
                    }
                }

                // L-stick押下時
                if report.buttons.middle.lstick() {
                    if !lstick {
                        enigo.button(Button::Middle, Press)?;
                        lstick = true;
                    }
                } else {
                    if lstick {
                        enigo.button(Button::Middle, Release)?;
                        lstick = false;
                    }
                }

                // R-stick押下時
                if report.buttons.middle.rstick() {
                    if !rstick {
                        enigo.key(Key::Return, Press)?;
                        should_sleep = true;
                        rstick = true;
                    }
                } else {
                    if rstick {
                        enigo.key(Key::Return, Release)?;
                        should_sleep = true;
                        rstick = false;
                    }
                }

                // Xボタン押下時
                if report.buttons.right.x() {
                    if !x {
                        enigo.key(Key::Meta, Press)?;
                        should_sleep = true;
                        x = true;
                    }
                } else {
                    if x {
                        enigo.key(Key::Meta, Release)?;
                        should_sleep = true;
                        x = false;
                    }
                }

                // Yボタン押下時
                if report.buttons.right.y() {
                    if !y {
                        enigo.key(Key::Shift, Press)?;
                        should_sleep = true;
                        y = true;
                    }
                } else {
                    if y {
                        enigo.key(Key::Shift, Release)?;
                        should_sleep = true;
                        y = false;
                    }
                }

                // ZRボタン押下時
                if report.buttons.right.zr() {
                    if !zr {
                        zr = true;
                    }
                } else {
                    if zr {
                        enigo.button(Button::Left, Click)?;
                        zr = false;
                    }
                }

                // Lボタン押下時
                if report.buttons.left.l() {
                    if !l {
                        enigo.key(Key::Alt, Press)?;
                        should_sleep = true;
                        l = true;
                    }
                } else {
                    if l {
                        enigo.key(Key::Alt, Release)?;
                        should_sleep = true;
                        l = false;
                    }
                }

                // Rボタン押下時
                if report.buttons.right.r() {
                    if !r {
                        r = true;
                    }
                } else {
                    if r {
                        enigo.button(Button::Right, Click)?;
                        r = false;
                    }
                }

                // ZLボタン押下時
                if report.buttons.left.zl() {
                    if !zl {
                        enigo.button(Button::Left, Press)?;
                        zl = true;
                    }
                } else {
                    if zl {
                        enigo.button(Button::Left, Release)?;
                        zl = false;
                    }
                }

                // スリープ中も UI スレッドがマウスを動かせるよう、先に Enigo を解放する。
                drop(enigo);

                // キー入力の切れ目でスリープしないと、マシンスペックによって順番が前後してしまう。
                if should_sleep {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });

        // UI更新スレッド(リフレッシュレートより速い)
        s.spawn(move || {
            // ホイール速度
            let mut vlx = 0.0;
            let mut vly = 0.0;

            // マウス速度
            let mut vrx = 0.0;
            let mut vry = 0.0;

            // ホイール端数
            let mut flx = 0.0;
            let mut fly = 0.0;

            // マウス端数
            let mut frx = 0.0;
            let mut fry = 0.0;

            loop {
                // 割り込み
                if *_interrupt.lock().unwrap() {
                    break;
                }

                // ホイール速度の調整(ドリフト防止のため微量のスティックは無視)
                let uslx = *slx.lock().unwrap();
                let usly = *sly.lock().unwrap();
                vlx = (vlx + (if uslx.abs() < 0.2 { 0.0 } else { uslx }) * stick / 16.0) * 0.9;
                vly = (vly - (if usly.abs() < 0.2 { 0.0 } else { usly }) * stick / 16.0) * 0.9;

                // マウス速度の調整(ドリフト防止のため微量のスティックは無視)
                let usrx = *srx.lock().unwrap();
                let usry = *sry.lock().unwrap();
                vrx = (vrx + (if usrx.abs() < 0.2 { 0.0 } else { usrx }) * stick * 2.0) * 0.9;
                vry = (vry - (if usry.abs() < 0.2 { 0.0 } else { usry }) * stick * 2.0) * 0.9;

                // モーション分
                let radians = rot.lock().unwrap().to_radians();
                let ugz = *gz.lock().unwrap();
                let ugy = *gy.lock().unwrap();
                // ドリフト防止のため微量のモーションは無視
                let nugz = if ugz.abs() < 2.0 { 0.0 } else { ugz };
                let nugy = if ugy.abs() < 2.0 { 0.0 } else { ugy };
                let cos = radians.cos();
                let sin = radians.sin();
                let mx = (nugz * cos + nugy * sin) * gyro / 8.0;
                let my = (nugz * sin - nugy * cos) * gyro / 8.0;

                // 最終的なホイール移動量
                let dlx = vlx + flx;
                let dly = vly + fly;

                // 最終的なマウス移動量
                let drx = vrx + mx + frx;
                let dry = vry + my + fry;

                // 端数を持ち越し
                let rdlx = dlx.round();
                let rdly = dly.round();
                flx = dlx - rdlx;
                fly = dly - rdly;

                let rdrx = drx.round();
                let rdry = dry.round();
                frx = drx - rdrx;
                fry = dry - rdry;

                {
                    let mut enigo = enigo.lock().unwrap();

                    // ホイール(enigo 0.6 は移動量 0 でもスクロールイベントを送るため、0 のときは呼ばない)
                    if rdlx != 0.0 {
                        let _ = enigo.scroll(rdlx as i32, Axis::Horizontal);
                    }
                    if rdly != 0.0 {
                        let _ = enigo.scroll(rdly as i32, Axis::Vertical);
                    }

                    // マウス移動
                    let _ = enigo.move_mouse(rdrx as i32, rdry as i32, Coordinate::Rel);
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
