mod config;
mod launch_agent;
mod motion;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{Action, Buttons, Sensitivity};
use enigo::{
    Axis, Coordinate,
    Direction::{Click, Press, Release},
    Enigo, InputResult, Keyboard, Mouse, NewConError, Settings,
};
use joycon::{
    BatteryLevel, Button, JoyCon, PlayerLight,
    hidapi::{BusType, DeviceInfo, HidApi, HidDevice},
};
use motion::{
    GYRO_PIXELS_PER_DEGREE, MAX_INPUT_GAP, Motion, Tracker, Velocity, radial_deadzone,
    snapped_deadzone,
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
    time::{Duration, Instant},
};

/// マウスとホイールを動かす間隔(リフレッシュレートより速い)。
/// 動かす量は実際に経った時間から計算するので、sleep が長引いても速さは変わらない。
const MOUSE_INTERVAL: Duration = Duration::from_millis(5);
/// キーを送った後、次のボタン入力を送るまで待つ時間。
/// キー入力の切れ目で待たないと、マシンスペックによって順番が前後してしまう。
const KEY_INTERVAL: Duration = Duration::from_millis(100);
/// コントローラーが見つからないときや、接続が切れたときに探し直す間隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// ホイールの速さ(L スティックを倒しきったときの加速の量、px、感度の倍率 1 あたり)。
///
/// ホイールはピクセル単位で送る。行単位(macOS では 1 行 10px)だと 1 行ずつ飛ぶので、
/// 軽く倒したときに 1 行分たまるたびにガクッと動いてしまう。ピクセル単位のイベントは
/// macOS が連続したスクロール(トラックパッドと同じ扱い)とするので、1px ずつ滑らかに動く。
/// 行単位で送っていた頃は 1/16 行(10/16 px)だったが、ピクセル単位では遅く感じたので 2 倍にした。
const WHEEL_SPEED: f64 = 20.0 / 16.0;

/// ポインタを画面中央へ戻すアニメーションの長さ(MOUSE_INTERVAL × 40 = 200ms)。
const GLIDE_STEPS: u32 = 40;

const BUTTON_COUNT: usize = 18;

/// ボタンと、設定で割り当てた操作の対応。同時に変化したボタンはこの順に送る。
fn bindings(buttons: &Buttons) -> [(Button, &Action); BUTTON_COUNT] {
    [
        (Button::A, &buttons.a),
        (Button::B, &buttons.b),
        (Button::Minus, &buttons.minus),
        (Button::Plus, &buttons.plus),
        (Button::Left, &buttons.left),
        (Button::Right, &buttons.right),
        (Button::Down, &buttons.down),
        (Button::Up, &buttons.up),
        (Button::Home, &buttons.home),
        (Button::Capture, &buttons.capture),
        (Button::LStick, &buttons.lstick),
        (Button::RStick, &buttons.rstick),
        (Button::X, &buttons.x),
        (Button::Y, &buttons.y),
        (Button::ZR, &buttons.zr),
        (Button::L, &buttons.l),
        (Button::R, &buttons.r),
        (Button::ZL, &buttons.zl),
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

/// 押しているボタン。
/// drop 時(切断・エラー・パニック)には、ボタンで押したままのキーとマウスボタンを離す。
/// ⌘ などが残ると、再接続後のクリック等にも付いてしまう。
struct Held<'a> {
    enigo: &'a Mutex<Enigo>,
    bindings: [(Button, &'a Action); BUTTON_COUNT],
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
    fn update(&mut self, buttons: joycon::Buttons) -> InputResult<bool> {
        let mut enigo = lock(self.enigo);
        let mut sent_keys = false;
        for i in 0..BUTTON_COUNT {
            let (button, action) = self.bindings[i];
            let pressed = buttons.is_pressed(button);
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

    // 想定外のパニックが再接続のたびに繰り返されることもあるので、パニックも重複を省いてログに出す。
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

        // 接続の失敗や切断はエラーで返るが、想定外のパニックでも止まらず再接続する(内容はパニックフックがログに出す)。
        // パニックやエラーの後は device を捨てて再接続するだけなので、AssertUnwindSafe でも問題ない。
        let result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            let joycon = JoyCon::new(device, &device_info)?;
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
        .filter(|x| joycon::is_supported(x))
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
    let battery = joycon.read()?.battery;
    joycon.set_player_lights([
        (battery >= BatteryLevel::Full).into(),
        (battery >= BatteryLevel::Medium).into(),
        (battery >= BatteryLevel::Low).into(),
        if battery >= BatteryLevel::Low {
            PlayerLight::On
        } else {
            PlayerLight::Blinking
        },
    ])?;

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
    buttons: Sender<joycon::Buttons>,
) -> Result<()> {
    let mut tracker = Tracker::default();
    loop {
        let report = joycon.read()?;
        tracker.update(&mut lock(motion), &report, Instant::now());

        buttons
            .send(report.buttons)
            .context("the button thread has stopped")?;
    }
}

/// 受け取ったボタンの状態から、押した・離したボタンの操作を送る。
fn send_buttons(buttons: Receiver<joycon::Buttons>, mut held: Held) -> Result<()> {
    for pressed in buttons {
        // update が Enigo を解放してから待つので、待っている間もマウスは動く。
        if held.update(pressed)? {
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
    // モーションは 1 度回したときのピクセル数にしておく。
    let gyro = sensitivity.gyro_factor() * GYRO_PIXELS_PER_DEGREE;
    let stick = sensitivity.stick_factor();
    let mut wheel_x = Velocity::default();
    let mut wheel_y = Velocity::default();
    let mut mouse_x = Velocity::default();
    let mut mouse_y = Velocity::default();
    // 中央へ戻している最中のアニメーション
    let mut glide: Option<Glide> = None;
    let mut last = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        // 前回から経った時間(止められていた後に、一度に大きく動かないよう上限を設ける)
        let now = Instant::now();
        let elapsed = now
            .saturating_duration_since(last)
            .min(MAX_INPUT_GAP)
            .as_secs_f64();
        last = now;
        let ([left, right], [turn_x, turn_y]) = {
            let mut motion = lock(motion);
            (motion.sticks(now), motion.take_turn(now, elapsed))
        };

        // スティック分(上に倒すと y が正なので、画面の向きに合わせて反転する)
        // どちらも斜めに動かせるが、ホイールは縦横の近くではその向きにそろえる。
        let [lx, ly] = snapped_deadzone(left);
        let [rx, ry] = radial_deadzone(right);
        let dlx = wheel_x.step(lx * stick * WHEEL_SPEED, elapsed, 0.0);
        let dly = wheel_y.step(-ly * stick * WHEEL_SPEED, elapsed, 0.0);
        // モーション分(読み取りスレッドで、コントローラーの姿勢に合わせて回してある)
        let drx = mouse_x.step(rx * stick * 2.0, elapsed, turn_x * gyro);
        let dry = mouse_y.step(-ry * stick * 2.0, elapsed, turn_y * gyro);

        {
            let mut enigo = lock(enigo);
            // enigo 0.6 は移動量 0 でもイベントを送るため、0 のときは呼ばない。
            // (止まっている間もマウスを動かしたことになり、スリープやスクリーンセーバーが効かなくなる)
            if dlx != 0.0 {
                let _ = enigo.smooth_scroll(dlx as i32, Axis::Horizontal);
            }
            if dly != 0.0 {
                let _ = enigo.smooth_scroll(dly as i32, Axis::Vertical);
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
                    if drx != 0.0 || dry != 0.0 {
                        let _ = enigo.move_mouse(drx as i32, dry as i32, Coordinate::Rel);
                    }
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
