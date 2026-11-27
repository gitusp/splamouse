use anyhow::{Context, Result};
use clap::Parser;
use enigo::{
    Axis, Button, Coordinate,
    Direction::{Click, Press, Release},
    Enigo, InputResult, Key, Keyboard, Mouse, NewConError, Settings,
};
use joycon::{
    IMU, JoyCon,
    hidapi::{BusType, DeviceInfo, HidApi},
    joycon_sys::{
        HID_IDS, NINTENDO_VENDOR_ID,
        input::{BatteryLevel, ButtonsStatus},
        light::{self, PlayerLight},
    },
};
use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

/// マウスとホイールを動かす間隔(リフレッシュレートより速い)。
const MOUSE_INTERVAL: Duration = Duration::from_millis(5);
/// キーを送った後、次のボタン入力を送るまで待つ時間。
/// キー入力の切れ目で待たないと、マシンスペックによって順番が前後してしまう。
const KEY_INTERVAL: Duration = Duration::from_millis(100);
/// コントローラーが見つからないときに探し直す間隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// ドリフト防止のため、これより小さいスティックの傾きは無視する。
const STICK_DEADZONE: f64 = 0.2;
/// ドリフト防止のため、これより小さいモーション(角速度)は無視する。
const GYRO_DEADZONE: f64 = 2.0;

// 配列によらず同じ位置のキーを押すよう、文字ではなく仮想キーコード(kVK_ANSI_*)で指定する。
const KEY_C: Key = Key::Other(0x08);
const KEY_R: Key = Key::Other(0x0F);
const KEY_V: Key = Key::Other(0x09);
const KEY_W: Key = Key::Other(0x0D);
const KEY_Z: Key = Key::Other(0x06);

/// ボタンに割り当てる操作。
#[derive(Clone, Copy)]
enum Action {
    /// 押している間キーを押す。離すときは押したのと逆順に離す。
    Keys(&'static [Key]),
    /// 押している間マウスボタンを押す。
    Hold(Button),
    /// 離したときにクリックする。
    ClickOnRelease(Button),
}

impl Action {
    fn send(self, enigo: &mut Enigo, pressed: bool) -> InputResult<()> {
        match (self, pressed) {
            (Self::Keys(keys), true) => keys.iter().try_for_each(|&key| enigo.key(key, Press)),
            (Self::Keys(keys), false) => keys
                .iter()
                .rev()
                .try_for_each(|&key| enigo.key(key, Release)),
            (Self::Hold(button), true) => enigo.button(button, Press),
            (Self::Hold(button), false) => enigo.button(button, Release),
            (Self::ClickOnRelease(_), true) => Ok(()),
            (Self::ClickOnRelease(button), false) => enigo.button(button, Click),
        }
    }
}

/// ボタンが押されているか。
type IsPressed = fn(&ButtonsStatus) -> bool;

/// ボタンと操作の対応。同時に変化したボタンはこの順に送る。
#[rustfmt::skip]
const BINDINGS: [(IsPressed, Action); 18] = {
    use Action::{ClickOnRelease, Hold, Keys};
    [
        (|b| b.right.a(), Keys(&[Key::Meta, Key::RightArrow])),
        (|b| b.right.b(), Keys(&[Key::Meta, Key::LeftArrow])),
        (|b| b.middle.minus(), Keys(&[Key::Meta, KEY_W])),
        (|b| b.middle.plus(), Keys(&[Key::Meta, KEY_R])),
        (|b| b.left.left(), Keys(&[Key::Control, Key::Shift, Key::Tab])),
        (|b| b.left.right(), Keys(&[Key::Control, Key::Tab])),
        (|b| b.left.down(), Keys(&[Key::Meta, KEY_C])),
        (|b| b.left.up(), Keys(&[Key::Meta, KEY_V])),
        (|b| b.middle.home(), Keys(&[Key::Meta, Key::Shift, KEY_Z])),
        (|b| b.middle.capture(), Keys(&[Key::Meta, KEY_Z])),
        (|b| b.middle.lstick(), Hold(Button::Middle)),
        (|b| b.middle.rstick(), Keys(&[Key::Return])),
        (|b| b.right.x(), Keys(&[Key::Meta])),
        (|b| b.right.y(), Keys(&[Key::Shift])),
        (|b| b.right.zr(), ClickOnRelease(Button::Left)),
        (|b| b.left.l(), Keys(&[Key::Alt])),
        (|b| b.right.r(), ClickOnRelease(Button::Right)),
        (|b| b.left.zl(), Hold(Button::Left)),
    ]
};

#[derive(Parser)]
struct Opts {
    #[arg(short, long, default_value = "0.0")]
    gyro: f64,
    #[arg(short, long, default_value = "0.0")]
    stick: f64,
}

/// 感度の倍率。
#[derive(Clone, Copy)]
struct Sensitivity {
    gyro: f64,
    stick: f64,
}

impl Sensitivity {
    /// -5.0〜5.0 で指定された感度を倍率にする。範囲外なら `None`。
    fn from_opts(opts: &Opts) -> Option<Self> {
        let range = -5.0..=5.0;
        (range.contains(&opts.gyro) && range.contains(&opts.stick)).then_some(Self {
            gyro: 2.0 + opts.gyro * 0.2,
            stick: 2.0 + opts.stick * 0.2,
        })
    }
}

/// センサーから読んだ最新の値。
#[derive(Clone, Copy, Default)]
struct Motion {
    /// L スティックの傾き (x, y)
    left_stick: [f64; 2],
    /// R スティックの傾き (x, y)
    right_stick: [f64; 2],
    /// ジャイロの角速度 (y, z)
    gyro: [f64; 2],
    /// コントローラーの姿勢(度)
    tilt: f64,
}

/// スティックで動かす 1 方向の速度。
#[derive(Default)]
struct Velocity {
    speed: f64,
    /// 整数に丸めた端数(次回に持ち越す)
    remainder: f64,
}

impl Velocity {
    /// スティックの入力で加速する。入力が無くなっても少しずつ減速する。
    fn accelerate(&mut self, input: f64) {
        self.speed = (self.speed + input) * 0.9;
    }

    /// 今回の移動量として、速度に `extra` を足して整数に丸めた値を返す。
    fn step(&mut self, extra: f64) -> f64 {
        let distance = self.speed + extra + self.remainder;
        let rounded = distance.round();
        self.remainder = distance - rounded;
        rounded
    }
}

/// drop されたとき(パニックの巻き戻しも含む)にフラグを立てる。
struct SetOnDrop<'a>(&'a AtomicBool);

impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn main() -> Result<()> {
    let opts = Opts::parse();
    let Some(sensitivity) = Sensitivity::from_opts(&opts) else {
        eprintln!("gyro and stick must be between -5.0 and 5.0");
        return Ok(());
    };

    // ⌘+クリックや ⌘+スクロールが効くよう、ボタンとマウスの両スレッドで1つの Enigo を共有して修飾キーの状態を揃える。
    // Enigo は drop 時に送ったイベント数に比例してスリープする(5ms 毎のマウス移動だと 1 時間で約 3 時間)ので、
    // 接続ごとに作り直さず、drop しないよう 'static にして使い回す。
    let enigo: &'static Mutex<Enigo> = Box::leak(Box::new(Mutex::new(new_enigo()?)));

    let mut api = HidApi::new()?;
    loop {
        api.refresh_devices()?;
        let Some(device_info) = find_controller(&api) else {
            eprintln!("No device found");
            thread::sleep(RETRY_INTERVAL);
            continue;
        };
        let device = device_info
            .open_device(&api)
            .with_context(|| format!("error opening the HID device {device_info:?}"))?;
        let device_info = device_info.clone();

        // NOTE: 接続が中途半端な際、よくパニックする。
        // パニックやエラーの後は device を捨てて再接続するだけなので、結果は捨て、AssertUnwindSafe でも問題ない。
        let _ = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            let joycon = JoyCon::new(device, device_info)?;
            eprintln!(
                "Connected via {}",
                if joycon.is_usb() { "USB" } else { "Bluetooth" }
            );
            run(joycon, enigo, sensitivity)
        }));
        release_held_keys(enigo);
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
                thread::sleep(RETRY_INTERVAL);
            }
            Err(e) => return Err(e).context("error initializing the input simulator"),
        }
    }
}

/// 接続されているコントローラーを探す。USB と Bluetooth の両方で見えている場合は USB を優先する。
fn find_controller(api: &HidApi) -> Option<&DeviceInfo> {
    api.device_list()
        .filter(|x| x.vendor_id() == NINTENDO_VENDOR_ID && HID_IDS.contains(&x.product_id()))
        .min_by_key(|x| !matches!(x.bus_type(), BusType::Usb))
}

/// ロックを取る。他のスレッドがパニックしても中身は使い続けられるので、ポイズンは無視する。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 押しっぱなしのキーを離す。切断時に ⌘ などが残ると、再接続後のクリック等にも付いてしまう。
fn release_held_keys(enigo: &Mutex<Enigo>) {
    let mut enigo = lock(enigo);
    let (keys, _) = enigo.held();
    for key in keys {
        let _ = enigo.key(key, Release);
    }
}

/// 接続したコントローラーでマウスを操作する。切断などで読めなくなったら返る。
fn run(mut joycon: JoyCon, enigo: &Mutex<Enigo>, sensitivity: Sensitivity) -> Result<()> {
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

    joycon.enable_imu()?;
    joycon.load_calibration()?;

    // ボタンを送った後に待つ間も、コントローラーの読み取りとマウスの移動が止まらないよう別スレッドにする。
    let motion = Mutex::new(Motion::default());
    let stop = AtomicBool::new(false);
    let (buttons_tx, buttons_rx) = mpsc::channel();
    thread::scope(|s| {
        s.spawn(|| move_cursor(&motion, enigo, sensitivity, &stop));
        let buttons = s.spawn(move || send_buttons(buttons_rx, enigo));

        // 読み取りが(パニックも含めて)終わったら、マウスのスレッドを止める。
        // ボタンのスレッドは、buttons_tx が drop されると受け取り済みの分を送り終えてから止まる。
        let read = {
            let _stop = SetOnDrop(&stop);
            read_controller(&mut joycon, &motion, buttons_tx)
        };
        let send = buttons.join().unwrap_or_else(|e| panic::resume_unwind(e));
        // ボタンのスレッドが先に失敗すると読み取りも送信エラーで終わるので、そちらを優先して返す。
        send.and(read)
    })
}

/// コントローラーを読み続け、スティックとモーションを `motion` に書き、ボタンを `buttons` に送る。
fn read_controller(
    joycon: &mut JoyCon,
    motion: &Mutex<Motion>,
    buttons: Sender<ButtonsStatus>,
) -> Result<()> {
    let mut latest = Motion::default();
    loop {
        let report = joycon.tick()?;

        latest.left_stick = [report.left_stick.x, report.left_stick.y];
        latest.right_stick = [report.right_stick.x, report.right_stick.y];
        // 遅れて届いたサブコマンドの応答などモーションの無いレポートでは、前回の値を使い続ける。
        for frame in report.imu.iter().flatten() {
            latest.gyro = [frame.gyro.y, frame.gyro.z];
            // コントローラーの姿勢(加速度センサでドリフト修正)
            let accel_tilt = (frame.accel.y / frame.accel.z).atan().to_degrees();
            latest.tilt =
                (latest.tilt - frame.gyro.x * IMU::SAMPLE_DURATION) * 0.95 + accel_tilt * 0.05;
        }
        *lock(motion) = latest;

        buttons
            .send(report.buttons)
            .context("the button thread has stopped")?;
    }
}

/// 受け取ったボタンの状態から、押した・離したボタンの操作を送る。
fn send_buttons(buttons: Receiver<ButtonsStatus>, enigo: &Mutex<Enigo>) -> Result<()> {
    let mut held = [false; BINDINGS.len()];
    for status in buttons {
        let mut sent_keys = false;
        {
            let mut enigo = lock(enigo);
            for ((is_pressed, action), held) in BINDINGS.iter().zip(&mut held) {
                let pressed = is_pressed(&status);
                if pressed != *held {
                    action.send(&mut enigo, pressed)?;
                    *held = pressed;
                    sent_keys |= matches!(action, Action::Keys(_));
                }
            }
        }
        // 待っている間もマウスを動かせるよう、Enigo を解放してから待つ。
        if sent_keys {
            thread::sleep(KEY_INTERVAL);
        }
    }
    Ok(())
}

/// `stop` が立つまで、スティックとモーションでマウスとホイールを動かし続ける。
fn move_cursor(
    motion: &Mutex<Motion>,
    enigo: &Mutex<Enigo>,
    sensitivity: Sensitivity,
    stop: &AtomicBool,
) {
    let Sensitivity { gyro, stick } = sensitivity;
    let mut wheel_x = Velocity::default();
    let mut wheel_y = Velocity::default();
    let mut mouse_x = Velocity::default();
    let mut mouse_y = Velocity::default();

    while !stop.load(Ordering::Relaxed) {
        let Motion {
            left_stick: [lx, ly],
            right_stick: [rx, ry],
            gyro: [gy, gz],
            tilt,
        } = *lock(motion);

        // スティック分(上に倒すと y が正なので、画面の向きに合わせて反転する)
        wheel_x.accelerate(deadzone(lx, STICK_DEADZONE) * stick / 16.0);
        wheel_y.accelerate(-deadzone(ly, STICK_DEADZONE) * stick / 16.0);
        mouse_x.accelerate(deadzone(rx, STICK_DEADZONE) * stick * 2.0);
        mouse_y.accelerate(-deadzone(ry, STICK_DEADZONE) * stick * 2.0);

        // モーション分(コントローラーの姿勢に合わせて回転する)
        let gy = deadzone(gy, GYRO_DEADZONE);
        let gz = deadzone(gz, GYRO_DEADZONE);
        let (sin, cos) = tilt.to_radians().sin_cos();
        let mx = (gz * cos + gy * sin) * gyro / 8.0;
        let my = (gz * sin - gy * cos) * gyro / 8.0;

        let (dlx, dly) = (wheel_x.step(0.0), wheel_y.step(0.0));
        let (drx, dry) = (mouse_x.step(mx), mouse_y.step(my));

        {
            let mut enigo = lock(enigo);
            // enigo 0.6 は移動量 0 でもスクロールイベントを送るため、0 のときは呼ばない。
            if dlx != 0.0 {
                let _ = enigo.scroll(dlx as i32, Axis::Horizontal);
            }
            if dly != 0.0 {
                let _ = enigo.scroll(dly as i32, Axis::Vertical);
            }
            let _ = enigo.move_mouse(drx as i32, dry as i32, Coordinate::Rel);
        }

        thread::sleep(MOUSE_INTERVAL);
    }
}

/// 絶対値が `threshold` 未満の値を 0 にする。
fn deadzone(value: f64, threshold: f64) -> f64 {
    if value.abs() < threshold { 0.0 } else { value }
}
