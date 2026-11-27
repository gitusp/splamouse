mod config;
mod launch_agent;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{Action, Buttons, Sensitivity};
use enigo::{
    Axis, Coordinate,
    Direction::{Click, Press, Release},
    Enigo, InputResult, Keyboard, Mouse, NewConError, Settings,
};
use joycon::{
    IMU, JoyCon,
    hidapi::{BusType, DeviceInfo, HidApi, HidDevice},
    joycon_sys::{
        JOYCON_L_BT, JOYCON_R_BT, NINTENDO_VENDOR_ID, PRO_CONTROLLER,
        input::{BatteryLevel, ButtonsStatus},
        light::{self, PlayerLight},
    },
};
use std::{
    io::{self, Write},
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
/// 対応しているコントローラー。
/// 充電グリップは joycon クレートが対応しておらず `JoyCon::new` がパニックするので含めない。
const PRODUCT_IDS: [u16; 3] = [JOYCON_L_BT, JOYCON_R_BT, PRO_CONTROLLER];
/// コントローラーが見つからないときや、接続が切れたときに探し直す間隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// ドリフト防止のため、これより小さいスティックの傾きは無視する。
const STICK_DEADZONE: f64 = 0.2;
/// ドリフト防止のため、これより小さいモーション(角速度)は無視する。
const GYRO_DEADZONE: f64 = 2.0;

/// ポインタを画面中央へ戻すアニメーションの長さ(MOUSE_INTERVAL × 40 = 200ms)。
const GLIDE_STEPS: u32 = 40;

/// ボタンが押されているか。
type IsPressed = fn(&ButtonsStatus) -> bool;

const BUTTON_COUNT: usize = 18;

/// ボタンと、設定で割り当てた操作の対応。同時に変化したボタンはこの順に送る。
fn bindings(buttons: &Buttons) -> [(IsPressed, &Action); BUTTON_COUNT] {
    [
        (|b| b.right.a(), &buttons.a),
        (|b| b.right.b(), &buttons.b),
        (|b| b.middle.minus(), &buttons.minus),
        (|b| b.middle.plus(), &buttons.plus),
        (|b| b.left.left(), &buttons.left),
        (|b| b.left.right(), &buttons.right),
        (|b| b.left.down(), &buttons.down),
        (|b| b.left.up(), &buttons.up),
        (|b| b.middle.home(), &buttons.home),
        (|b| b.middle.capture(), &buttons.capture),
        (|b| b.middle.lstick(), &buttons.lstick),
        (|b| b.middle.rstick(), &buttons.rstick),
        (|b| b.right.x(), &buttons.x),
        (|b| b.right.y(), &buttons.y),
        (|b| b.right.zr(), &buttons.zr),
        (|b| b.left.l(), &buttons.l),
        (|b| b.right.r(), &buttons.r),
        (|b| b.left.zl(), &buttons.zl),
    ]
}

#[derive(Parser)]
struct Opts {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run splamouse in the background now and at every login
    Install,
    /// Stop running splamouse in the background
    Uninstall,
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

/// 押しているボタン。
/// drop 時(切断・エラー・パニック)には、ボタンで押したままのキーとマウスボタンを離す。
/// ⌘ などが残ると、再接続後のクリック等にも付いてしまう。
struct Held<'a> {
    enigo: &'a Mutex<Enigo>,
    bindings: [(IsPressed, &'a Action); BUTTON_COUNT],
    /// 中央へ戻す要求(マウスのスレッドが受け取る)
    center: &'a AtomicBool,
    /// `bindings` の各ボタンを押しているか
    pressed: [bool; BUTTON_COUNT],
}

impl<'a> Held<'a> {
    fn new(enigo: &'a Mutex<Enigo>, buttons: &'a Buttons, center: &'a AtomicBool) -> Self {
        Self {
            enigo,
            bindings: bindings(buttons),
            center,
            pressed: [false; BUTTON_COUNT],
        }
    }

    /// 押した・離したボタンの操作を送る。キーを送ったら true を返す。
    fn update(&mut self, status: ButtonsStatus) -> InputResult<bool> {
        let mut enigo = lock(self.enigo);
        let mut sent_keys = false;
        for i in 0..BUTTON_COUNT {
            let (is_pressed, action) = self.bindings[i];
            let pressed = is_pressed(&status);
            if pressed == self.pressed[i] {
                continue;
            }
            if pressed {
                self.press(&mut enigo, i)?;
            } else {
                self.release(&mut enigo, i)?;
            }
            sent_keys |= matches!(action, Action::Keys(_));
        }
        Ok(sent_keys)
    }

    fn press(&mut self, enigo: &mut Enigo, i: usize) -> InputResult<()> {
        match *self.bindings[i].1 {
            Action::Keys(ref keys) => keys.iter().try_for_each(|&key| enigo.key(key, Press))?,
            Action::Hold(button) => enigo.button(button, Press)?,
            Action::Center => self.center.store(true, Ordering::Relaxed),
            Action::ClickOnRelease(_) | Action::None => {}
        }
        self.pressed[i] = true;
        Ok(())
    }

    /// 他のボタンでも押しているキーやマウスボタンは、離さない・クリックしない。
    /// (X(⌘)を押したまま A(⌘ + →)を離したときの ⌘ や、ZL でドラッグ中に ZR を離したときの左ボタンなど)
    fn release(&mut self, enigo: &mut Enigo, i: usize) -> InputResult<()> {
        self.pressed[i] = false;
        match *self.bindings[i].1 {
            Action::Keys(ref keys) => {
                for &key in keys.iter().rev() {
                    if !self.any_pressed(|action| action.holds_key(key)) {
                        enigo.key(key, Release)?;
                    }
                }
            }
            Action::Hold(button) => {
                if !self.any_pressed(|action| action.holds_button(button)) {
                    enigo.button(button, Release)?;
                }
            }
            Action::ClickOnRelease(button) => {
                if !self.any_pressed(|action| action.holds_button(button)) {
                    enigo.button(button, Click)?;
                }
            }
            Action::Center | Action::None => {}
        }
        Ok(())
    }

    /// 押しているボタンのどれかの操作が `f` を満たすか。
    fn any_pressed(&self, f: impl Fn(&Action) -> bool) -> bool {
        self.bindings
            .iter()
            .zip(self.pressed)
            .any(|(&(_, action), pressed)| pressed && f(action))
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut enigo = lock(self.enigo);
        for (i, (_, action)) in self.bindings.into_iter().enumerate() {
            // 切断などで離したことになるボタンでは、クリックしない。
            if self.pressed[i] && !matches!(action, Action::ClickOnRelease(_)) {
                let _ = self.release(&mut enigo, i);
            }
        }
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
    match Opts::parse().command {
        Some(Command::Install) => return launch_agent::install(),
        Some(Command::Uninstall) => return launch_agent::uninstall(),
        None => {}
    }

    // 接続が中途半端な間は同じパニックが繰り返されるので、パニックも重複を省いてログに出す。
    panic::set_hook(Box::new(|info| log_status(info.to_string())));

    // 設定ファイルに誤りがあれば、直されるまで待つ。
    // 終了すると、LaunchAgent では launchd が起動し直すたびに同じエラーがログに溜まってしまうため。
    let config = loop {
        match config::load() {
            Ok(config) => break config,
            Err(e) => {
                log_status(format!("{e:#}"));
                thread::sleep(RETRY_INTERVAL);
            }
        }
    };

    // ⌘+クリックや ⌘+スクロールが効くよう、ボタンとマウスの両スレッドで1つの Enigo を共有して修飾キーの状態を揃える。
    // Enigo は drop 時に送ったイベント数に比例してスリープする(5ms 毎のマウス移動だと 1 時間で約 3 時間)ので、
    // 接続ごとに作り直さず、drop しないよう 'static にして使い回す。
    let enigo: &'static Mutex<Enigo> = Box::leak(Box::new(Mutex::new(new_enigo()?)));

    let mut api = HidApi::new()?;
    loop {
        let (device, device_info) = match open_controller(&mut api) {
            Ok(found) => found,
            Err(e) => {
                log_status(format!("{e:#}"));
                thread::sleep(RETRY_INTERVAL);
                continue;
            }
        };

        // NOTE: 接続が中途半端な際、よくパニックする(内容はパニックフックがログに出す)。
        // パニックやエラーの後は device を捨てて再接続するだけなので、AssertUnwindSafe でも問題ない。
        let result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            let joycon = JoyCon::new(device, device_info)?;
            log_status(format!(
                "Connected via {}",
                if joycon.is_usb() { "USB" } else { "Bluetooth" }
            ));
            run(joycon, enigo, &config)
        }));
        if let Ok(Err(e)) = result {
            log_status(format!("Disconnected: {e:#}"));
        }
        // コントローラーが見えたまま接続に失敗し続けても CPU を使い切らないよう、待ってから探し直す。
        thread::sleep(RETRY_INTERVAL);
    }
}

/// 状態をログに出す。直前と同じ内容なら出さない。
/// launchd で常駐させると、未接続の間や接続失敗が続く間にログが同じ行で埋まってしまうため。
fn log_status(message: impl Into<String>) {
    static LAST: Mutex<String> = Mutex::new(String::new());
    let message = message.into();
    let mut last = lock(&LAST);
    if *last != message {
        // パニックフックからも呼ばれるので、書き込みに失敗してもパニックしない writeln! を使う。
        let _ = writeln!(io::stderr(), "{message}");
        *last = message;
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

/// 接続されているコントローラーを開く。USB と Bluetooth の両方で見えている場合は USB を優先する。
fn open_controller(api: &mut HidApi) -> Result<(HidDevice, DeviceInfo)> {
    api.refresh_devices()
        .context("Failed to list the HID devices")?;
    let info = api
        .device_list()
        .filter(|x| x.vendor_id() == NINTENDO_VENDOR_ID && PRODUCT_IDS.contains(&x.product_id()))
        .min_by_key(|x| !matches!(x.bus_type(), BusType::Usb))
        .context("No device found")?;
    let device = info.open_device(api).with_context(|| {
        format!(
            "Failed to open {}",
            info.product_string().unwrap_or("the controller")
        )
    })?;
    Ok((device, info.clone()))
}

/// ロックを取る。他のスレッドがパニックしても中身は使い続けられるので、ポイズンは無視する。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 接続したコントローラーでマウスを操作する。切断などで読めなくなったら返る。
fn run(mut joycon: JoyCon, enigo: &Mutex<Enigo>, config: &config::Config) -> Result<()> {
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
    let center = AtomicBool::new(false);
    let (buttons_tx, buttons_rx) = mpsc::channel();
    thread::scope(|s| {
        s.spawn(|| move_cursor(&motion, enigo, config.sensitivity, &center, &stop));
        let buttons =
            s.spawn(|| send_buttons(buttons_rx, Held::new(enigo, &config.buttons, &center)));

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
fn send_buttons(buttons: Receiver<ButtonsStatus>, mut held: Held) -> Result<()> {
    for status in buttons {
        // update が Enigo を解放してから待つので、待っている間もマウスは動く。
        if held.update(status)? {
            thread::sleep(KEY_INTERVAL);
        }
    }
    Ok(())
}

/// `stop` が立つまで、スティックとモーションでマウスとホイールを動かし続ける。
/// `center` が立ったら、ポインタを画面中央へ滑らかに戻す。
fn move_cursor(
    motion: &Mutex<Motion>,
    enigo: &Mutex<Enigo>,
    sensitivity: Sensitivity,
    center: &AtomicBool,
    stop: &AtomicBool,
) {
    let (gyro, stick) = (sensitivity.gyro_factor(), sensitivity.stick_factor());
    let mut wheel_x = Velocity::default();
    let mut wheel_y = Velocity::default();
    let mut mouse_x = Velocity::default();
    let mut mouse_y = Velocity::default();
    // 中央へ戻している最中のアニメーション
    let mut glide: Option<Glide> = None;

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
            // 中央へ戻す要求が来たら、その時点の位置からアニメーションを始める。
            if center.swap(false, Ordering::Relaxed) {
                glide = Glide::to_center(&enigo);
            }
            match glide.as_mut().and_then(Iterator::next) {
                Some((x, y)) => {
                    // 中央でぴったり止まるよう、移動中のスティックとモーションの分は捨てる。
                    mouse_x = Velocity::default();
                    mouse_y = Velocity::default();
                    let _ = enigo.move_mouse(x, y, Coordinate::Abs);
                }
                None => {
                    glide = None;
                    let _ = enigo.move_mouse(drx as i32, dry as i32, Coordinate::Rel);
                }
            }
        }

        thread::sleep(MOUSE_INTERVAL);
    }
}

/// 現在位置からメインディスプレイの中央へ、ease-out で動かす座標列。
struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    step: u32,
}

impl Glide {
    fn to_center(enigo: &Enigo) -> Option<Self> {
        let (x, y) = enigo.location().ok()?;
        let (width, height) = enigo.main_display().ok()?;
        Some(Self {
            from: (x as f64, y as f64),
            to: (width as f64 / 2.0, height as f64 / 2.0),
            step: 0,
        })
    }
}

impl Iterator for Glide {
    type Item = (i32, i32);

    fn next(&mut self) -> Option<Self::Item> {
        if self.step >= GLIDE_STEPS {
            return None;
        }
        self.step += 1;
        let t = f64::from(self.step) / f64::from(GLIDE_STEPS);
        // ease-out cubic: 素早く動き出して、中央でゆっくり止まる。
        let eased = 1.0 - (1.0 - t).powi(3);
        let lerp = |from: f64, to: f64| (from + (to - from) * eased).round() as i32;
        Some((lerp(self.from.0, self.to.0), lerp(self.from.1, self.to.1)))
    }
}

/// 絶対値が `threshold` 未満の値を 0 にする。
fn deadzone(value: f64, threshold: f64) -> f64 {
    if value.abs() < threshold { 0.0 } else { value }
}
