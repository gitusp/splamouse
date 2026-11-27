//! コントローラーの入力から、ポインタとホイールを動かす量を計算する。

use joycon::{IMU_SAMPLE_DURATION, Imu, Report, Vec3};
use std::time::{Duration, Instant};

/// 速さの基準にする間隔(秒)。
/// 以前は 5ms 毎に 1 回分動かすつもりで、macOS では 5ms の sleep が実際には平均約 7.2ms かかっていた
/// (M2 Pro / macOS 27 で計測)。感度はその速さで選ばれているので、移動量を実際に経った時間から
/// 計算するようにしても速さが変わらないよう、この間隔を 1 回分とする。
const REFERENCE_TICK: f64 = 0.0072;
/// スティックの速度に、基準の間隔ごとに掛ける減速の割合。
const FRICTION: f64 = 0.9;
/// モーションの、1 度回したときの移動量(ピクセル、感度の倍率 1 あたり)。
/// 以前の「基準の間隔ごとに 角速度 × 倍率 / 8」と同じ速さ。
pub const GYRO_PIXELS_PER_DEGREE: f64 = 1.0 / 8.0 / REFERENCE_TICK;

/// ドリフト防止のため、これより小さいスティックの傾きは無視する。
const STICK_DEADZONE: f64 = 0.2;
/// ホイールを縦か横にそろえる、軸からの角度(度)。
const WHEEL_SNAP: f64 = 20.0;
/// ドリフト防止のため、これより小さいモーション(角速度、度/秒)は無視する。
const GYRO_DEADZONE: f64 = 2.0;

/// これより長くレポートが途切れたら、最後のスティックの傾きで動き続けない。
/// また、マウスのスレッドが止められていても、一度に動かすのはこの時間分まで。
pub const MAX_INPUT_GAP: Duration = Duration::from_millis(100);

/// 加速度の大きさが 1G からこれ(G)以上ずれていたら(振っている最中など)、重力の向きとして使わない。
const ACCEL_TOLERANCE: f64 = 0.25;
/// 姿勢のずれを加速度センサで直す時定数(秒)。
const TILT_TIME_CONSTANT: f64 = 0.2;

/// レポートの間隔の見積もりの初期値と、それをレポート何個分の重みで扱うか。
const INITIAL_REPORT_INTERVAL: f64 = 3.0 * IMU_SAMPLE_DURATION;
const INITIAL_REPORT_WEIGHT: f64 = 3.0;
/// 間隔の平均を取るレポートの数の上限(それ以降は直近をこの数くらいで平均する)。
const REPORT_AVERAGING: f64 = 100.0;
/// 平均に入れる間隔の上限(秒)。これより長い途切れは、一時的なものとして平均に響かせない。
const MAX_REPORT_INTERVAL: f64 = 0.05;

/// 読み取りスレッドが書き、マウスのスレッドが読む入力。
#[derive(Default)]
pub struct Motion {
    /// L スティックの傾き (x, y)
    left_stick: [f64; 2],
    /// R スティックの傾き (x, y)
    right_stick: [f64; 2],
    /// まだポインタに流していない、モーションで動かす分(画面の向きに回した回転角 [横, 縦]、度)
    turn: [f64; 2],
    /// 最後にレポートを読んだ時刻
    read_at: Option<Instant>,
    /// 最後にモーションのあるレポートを読んだ時刻
    imu_at: Option<Instant>,
    /// 次のモーションのあるレポートまでの見込みの間隔(秒)
    interval: f64,
}

impl Motion {
    /// スティックの傾き (L, R)。レポートが途切れている間は、最後の傾きで動き続けないよう中立にする。
    pub fn sticks(&self, now: Instant) -> [[f64; 2]; 2] {
        let fresh = self
            .read_at
            .is_some_and(|at| now.saturating_duration_since(at) <= MAX_INPUT_GAP);
        if fresh {
            [self.left_stick, self.right_stick]
        } else {
            [[0.0; 2]; 2]
        }
    }

    /// モーションで動かす分のうち、前回から `elapsed` 秒経った今回に流す分を取り出す。
    /// 届いた分をまとめて動かすと、ポインタがレポートの間隔(15ms 前後)でカクつくので、
    /// 次のレポートが届くはずの時刻までに均等に流す。届くのが遅れても、届いた分より先は動かさない。
    pub fn take_turn(&mut self, now: Instant, elapsed: f64) -> [f64; 2] {
        let remaining = self.imu_at.map_or(0.0, |at| {
            self.interval - now.saturating_duration_since(at).as_secs_f64()
        });
        let share = if remaining > 0.0 {
            elapsed / (elapsed + remaining)
        } else {
            1.0
        };
        let taken = self.turn.map(|turn| turn * share);
        self.turn = [self.turn[0] - taken[0], self.turn[1] - taken[1]];
        taken
    }
}

/// 読み取りスレッドで、レポートから `Motion` を更新する。
#[derive(Default)]
pub struct Tracker {
    tilt: Tilt,
    interval: ReportInterval,
}

impl Tracker {
    /// `now` に読んだ `report` で `motion` を更新する。
    pub fn update(&mut self, motion: &mut Motion, report: &Report, now: Instant) {
        motion.left_stick = [report.left_stick.x, report.left_stick.y];
        motion.right_stick = [report.right_stick.x, report.right_stick.y];
        motion.read_at = Some(now);

        // 遅れて届いたサブコマンドの応答など、モーションの無いレポートでは動かさない。
        let Some(samples) = report.imu else { return };
        let interval = self.interval.update(now);
        let dt = interval / samples.len() as f64;
        // サンプルは古い順。1 つずつ、その時点の姿勢で画面の向きに回して積算する。
        for imu in &samples {
            self.tilt.update(imu, dt);
            let gy = deadzone(imu.gyro.y, GYRO_DEADZONE);
            let gz = deadzone(imu.gyro.z, GYRO_DEADZONE);
            let (sin, cos) = self.tilt.degrees().to_radians().sin_cos();
            motion.turn[0] += (gz * cos + gy * sin) * dt;
            motion.turn[1] += (gz * sin - gy * cos) * dt;
        }
        motion.imu_at = Some(now);
        motion.interval = interval;
    }
}

/// モーションのあるレポートが届く間隔の見積もり(秒)。1 サンプル分の時間はこの 1/3。
/// 間隔は繋ぎ方で変わり(Bluetooth で約 15ms)、サンプルが前のレポートと重なることもあるので、
/// 決め打ちにせず、実際に届いた間隔の平均から求める(SDL と同じ考え方)。
/// 取りこぼしたレポートの分も平均に入るので、長い目で見て積算する時間が実際の時間と揃う。
struct ReportInterval {
    average: f64,
    /// 平均に入れたレポートの数(初期値の重みを含む)
    weight: f64,
    last: Option<Instant>,
}

impl Default for ReportInterval {
    fn default() -> Self {
        Self {
            average: INITIAL_REPORT_INTERVAL,
            weight: INITIAL_REPORT_WEIGHT,
            last: None,
        }
    }
}

impl ReportInterval {
    /// `now` に届いたレポートを数えて、見積もりを返す。
    fn update(&mut self, now: Instant) -> f64 {
        if let Some(last) = self.last.replace(now) {
            let interval = now
                .saturating_duration_since(last)
                .as_secs_f64()
                .min(MAX_REPORT_INTERVAL);
            self.weight = (self.weight + 1.0).min(REPORT_AVERAGING);
            self.average += (interval - self.average) / self.weight;
        }
        self.average
    }
}

/// コントローラーを前後の軸(x)まわりに傾けている角度(度、-180〜180)。
/// ジャイロで追い、加速度センサで見た重力の向きでずれを直す。
///
/// レポートの軸は、水平に置くと加速度の z が +1G で、x まわりに右手の法則の向き(ジャイロの x が正)
/// に回すと y 側が持ち上がって、`atan2(y, z)` が増える(SDL と同じ軸の対応から)。
#[derive(Default)]
struct Tilt(f64);

impl Tilt {
    fn degrees(&self) -> f64 {
        self.0
    }

    /// `dt` 秒分のサンプルで更新する。
    fn update(&mut self, imu: &Imu, dt: f64) {
        let predicted = self.0 + imu.gyro.x * dt;
        let Vec3 { x, y, z } = imu.accel;
        let measured = y.atan2(z).to_degrees();
        // 重力以外の加速度が大きいときや、前後の軸が縦に近くて y と z から傾きが決まらないときは、
        // 加速度センサをあまり信用しない。
        let norm = (x * x + y * y + z * z).sqrt();
        let trust =
            (1.0 - (norm - 1.0).abs() / ACCEL_TOLERANCE).clamp(0.0, 1.0) * y.hypot(z).min(1.0);
        let weight = -(-dt / TILT_TIME_CONSTANT).exp_m1() * trust;
        self.0 = wrap_degrees(predicted + wrap_degrees(measured - predicted) * weight);
    }
}

/// 角度を -180〜180 度に収める。
fn wrap_degrees(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

/// スティックで動かす 1 方向の速度。
#[derive(Default)]
pub struct Velocity {
    /// 基準の間隔あたりの移動量
    speed: f64,
    /// 整数に丸めた端数(次回に持ち越す)
    remainder: f64,
}

impl Velocity {
    /// スティックの入力 `input` で `elapsed` 秒の間加速し、その間の移動量に `extra` を足して
    /// 整数に丸めた値を返す。入力が無くなっても少しずつ減速する。
    pub fn step(&mut self, input: f64, elapsed: f64, extra: f64) -> f64 {
        let distance = self.advance(input, elapsed) + extra + self.remainder;
        let rounded = distance.round();
        self.remainder = distance - rounded;
        rounded
    }

    /// `elapsed` 秒の間加速し、その間の移動量を返す。
    /// 基準の間隔ごとに `speed = (speed + input) * FRICTION` として speed ずつ進むのを、
    /// 間隔の長さによらず同じ結果になるよう、まとめて計算したもの。
    fn advance(&mut self, input: f64, elapsed: f64) -> f64 {
        let ticks = elapsed / REFERENCE_TICK;
        // 入力を続けると落ち着く速度
        let terminal = input * FRICTION / (1.0 - FRICTION);
        let decay = FRICTION.powf(ticks);
        let distance = terminal * ticks
            + (self.speed - terminal) * FRICTION * (1.0 - decay) / (1.0 - FRICTION);
        self.speed = terminal + (self.speed - terminal) * decay;
        distance
    }
}

/// 傾きの大きさが `STICK_DEADZONE` 未満なら 0 にし、そこから倒しきるまでを 0〜1 に伸ばす。向きは変えない。
/// 斜めにも動かせるので、ポインタに使う。
pub fn radial_deadzone([x, y]: [f64; 2]) -> [f64; 2] {
    let length = x.hypot(y);
    if length < STICK_DEADZONE {
        return [0.0; 2];
    }
    let scale = (length - STICK_DEADZONE) / (1.0 - STICK_DEADZONE) / length;
    [x * scale, y * scale]
}

/// `radial_deadzone` に加えて、縦か横から `WHEEL_SNAP` 度以内に倒したときはその軸にそろえる。
/// 縦にスクロールしているつもりで横に流れないようにしつつ、はっきり斜めに倒せば斜めにも動かせるので、
/// ホイールに使う。そろえる範囲の境目で向きが飛ばないよう、残りの角度を 0〜90 度に伸ばす。
pub fn snapped_deadzone(stick: [f64; 2]) -> [f64; 2] {
    let [x, y] = radial_deadzone(stick);
    let length = x.hypot(y);
    // 横からの角度(0〜90 度)
    let angle = y.abs().atan2(x.abs()).to_degrees();
    let angle = (angle - WHEEL_SNAP) / (90.0 - 2.0 * WHEEL_SNAP) * 90.0;
    if angle <= 0.0 {
        [length.copysign(x), 0.0]
    } else if angle >= 90.0 {
        [0.0, length.copysign(y)]
    } else {
        let (sin, cos) = angle.to_radians().sin_cos();
        [(length * cos).copysign(x), (length * sin).copysign(y)]
    }
}

/// 絶対値が `threshold` 未満の値を 0 にする。
fn deadzone(value: f64, threshold: f64) -> f64 {
    if value.abs() < threshold { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joycon::{BatteryLevel, Buttons, Stick};

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} is not within {tolerance} of {expected}"
        );
    }

    /// 前後の軸まわりに `tilt` 度傾けたときの重力(加速度センサの読み)。
    fn gravity(tilt: f64) -> Vec3 {
        let (sin, cos) = tilt.to_radians().sin_cos();
        Vec3 {
            x: 0.0,
            y: sin,
            z: cos,
        }
    }

    fn report(imu: Option<[Imu; 3]>) -> Report {
        Report {
            buttons: Buttons::default(),
            left_stick: Stick::default(),
            right_stick: Stick { x: 1.0, y: 0.0 },
            battery: BatteryLevel::Full,
            imu,
        }
    }

    #[test]
    fn velocity_matches_the_previous_steps() {
        let mut velocity = Velocity::default();
        let mut speed = 0.0;
        for i in 0..40 {
            let input = if i < 25 { 4.0 } else { 0.0 };
            speed = (speed + input) * FRICTION;
            assert_close(velocity.advance(input, REFERENCE_TICK), speed, 1e-9);
        }
    }

    #[test]
    fn velocity_does_not_depend_on_the_interval() {
        let (mut once, mut twice) = (Velocity::default(), Velocity::default());
        for input in [4.0, 4.0, -2.0, 0.0, 0.0] {
            let long = once.advance(input, 2.0 * REFERENCE_TICK);
            let short = twice.advance(input, REFERENCE_TICK) + twice.advance(input, REFERENCE_TICK);
            assert_close(long, short, 1e-9);
            assert_close(once.speed, twice.speed, 1e-9);
        }
        // 時間が経っていなければ動かない
        assert_eq!(once.advance(4.0, 0.0), 0.0);
    }

    #[test]
    fn velocity_carries_the_remainder() {
        let mut velocity = Velocity::default();
        let moved: f64 = (0..10)
            .map(|_| velocity.step(0.0, REFERENCE_TICK, 0.3))
            .sum();
        assert_eq!(moved, 3.0);
    }

    #[test]
    fn deadzones_start_from_zero() {
        assert_eq!(radial_deadzone([0.1, 0.1]), [0.0, 0.0]);
        assert_eq!(radial_deadzone([0.2, 0.0]), [0.0, 0.0]);
        assert_eq!(radial_deadzone([1.0, 0.0]), [1.0, 0.0]);
        let [x, y] = radial_deadzone([0.0, -0.6]);
        assert_eq!(x, 0.0);
        assert_close(y, -0.5, 1e-12);

        assert_eq!(snapped_deadzone([0.1, 0.1]), [0.0, 0.0]);
        assert_eq!(snapped_deadzone([0.0, 1.0]), [0.0, 1.0]);
        let [x, y] = snapped_deadzone([-0.6, 0.0]);
        assert_close(x, -0.5, 1e-12);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn radial_deadzone_keeps_diagonals() {
        // 軸ごとの判定だと、y が 0.2 未満なので真横になってしまう
        let [x, y] = radial_deadzone([0.6, 0.15]);
        assert!(y > 0.0);
        assert_close(y / x, 0.25, 1e-12);
    }

    /// 横から `degrees` 度の向きに、`length` だけ倒した傾き。
    fn stick_at(degrees: f64, length: f64) -> [f64; 2] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [length * cos, length * sin]
    }

    #[test]
    fn snapped_deadzone_snaps_near_an_axis() {
        // 縦のつもりで 15 度ずれても、横には動かない
        let [x, y] = snapped_deadzone(stick_at(90.0 + 15.0, 1.0));
        assert_eq!(x, 0.0);
        assert_close(y, 1.0, 1e-12);
        let [x, y] = snapped_deadzone(stick_at(-19.0, 0.6));
        assert_close(x, 0.5, 1e-12);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn snapped_deadzone_moves_diagonally() {
        for (x, y) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
            let [sx, sy] = snapped_deadzone(stick_at(f64::atan2(y, x).to_degrees(), 1.0));
            assert_close(sx, x * 0.5_f64.sqrt(), 1e-12);
            assert_close(sy, y * 0.5_f64.sqrt(), 1e-12);
        }
    }

    #[test]
    fn snapped_deadzone_turns_smoothly_past_the_snap() {
        // そろえる範囲を少し越えたところでは、まだほとんど軸の向き
        let [x, y] = snapped_deadzone(stick_at(WHEEL_SNAP + 1.0, 1.0));
        assert_close(
            y.atan2(x).to_degrees(),
            90.0 / (90.0 - 2.0 * WHEEL_SNAP),
            1e-9,
        );
        // 倒した量はそろえても変わらない
        assert_close(x.hypot(y), 1.0, 1e-12);
    }

    /// 静止した姿勢の加速度だけで、傾きを決める。
    fn settle(tilt: f64) -> Tilt {
        let mut estimate = Tilt::default();
        let imu = Imu {
            gyro: Vec3::default(),
            accel: gravity(tilt),
        };
        for _ in 0..1000 {
            estimate.update(&imu, IMU_SAMPLE_DURATION);
        }
        estimate
    }

    #[test]
    fn tilt_covers_the_full_turn() {
        for tilt in [0.0, 30.0, -60.0, 120.0, -150.0, 179.0] {
            assert_close(settle(tilt).degrees(), tilt, 0.01);
        }
    }

    #[test]
    fn tilt_stays_finite_without_gravity_on_y_and_z() {
        let mut tilt = settle(30.0);
        let before = tilt.degrees();
        let imu = Imu {
            gyro: Vec3::default(),
            accel: Vec3 {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        };
        for _ in 0..100 {
            tilt.update(&imu, IMU_SAMPLE_DURATION);
        }
        // 傾きが決まらない間は、前の値を保つ
        assert_close(tilt.degrees(), before, 1e-9);
    }

    #[test]
    fn tilt_follows_the_gyro_while_rolling() {
        // 90 度/秒で 1 回転させる間、遅れずに追う
        let mut tilt = Tilt::default();
        for i in 1..=800 {
            let angle = 90.0 * IMU_SAMPLE_DURATION * f64::from(i);
            let imu = Imu {
                gyro: Vec3 {
                    x: 90.0,
                    y: 0.0,
                    z: 0.0,
                },
                accel: gravity(angle),
            };
            tilt.update(&imu, IMU_SAMPLE_DURATION);
            assert_close(wrap_degrees(tilt.degrees() - angle), 0.0, 1e-6);
        }
    }

    #[test]
    fn tilt_ignores_acceleration_from_swinging() {
        let mut tilt = settle(0.0);
        // 横に振って、重力と合わせて 2G かかっている
        let imu = Imu {
            gyro: Vec3::default(),
            accel: Vec3 {
                x: 0.0,
                y: 3.0_f64.sqrt(),
                z: 1.0,
            },
        };
        for _ in 0..20 {
            tilt.update(&imu, IMU_SAMPLE_DURATION);
        }
        assert_close(tilt.degrees(), 0.0, 1e-9);
    }

    #[test]
    fn report_interval_follows_the_reports() {
        let mut interval = ReportInterval::default();
        let start = Instant::now();
        let mut estimate = 0.0;
        for i in 0..300 {
            estimate = interval.update(start + Duration::from_millis(8) * i);
        }
        assert_close(estimate, 0.008, 0.0001);

        // 一時的な途切れは、上限までしか平均に入れない
        let at = start + Duration::from_millis(8) * 299 + Duration::from_secs(1);
        assert_close(interval.update(at), 0.008, 0.001);
    }

    /// `tilt` 度傾けた姿勢で、真上から見て `rate` 度/秒で回した(正なら左を向く)ときのサンプル。
    /// 姿勢に応じて、ジャイロの y と z に分かれて出る。
    fn yaw(tilt: f64, rate: f64) -> Imu {
        let (sin, cos) = tilt.to_radians().sin_cos();
        Imu {
            gyro: Vec3 {
                x: 0.0,
                y: -rate * sin,
                z: -rate * cos,
            },
            accel: gravity(tilt),
        }
    }

    /// `tilt` 度傾けた姿勢で、`interval` 毎にレポートが届く中、`rate` 度/秒で回したときに
    /// 動かす分の、1 秒あたりの量。
    fn turn_per_second(tilt: f64, rate: f64, interval: Duration) -> [f64; 2] {
        let mut tracker = Tracker::default();
        let mut motion = Motion::default();
        let mut now = Instant::now();
        // 姿勢と間隔の見積もりが落ち着くまで、静止させておく
        let still = yaw(tilt, 0.0);
        for _ in 0..500 {
            tracker.update(&mut motion, &report(Some([still; 3])), now);
            now += interval;
        }
        motion.turn = [0.0; 2];

        let reports = 200;
        for _ in 0..reports {
            tracker.update(&mut motion, &report(Some([yaw(tilt, rate); 3])), now);
            now += interval;
        }
        let seconds = interval.as_secs_f64() * f64::from(reports);
        motion.turn.map(|turn| turn / seconds)
    }

    #[test]
    fn turning_moves_the_same_whatever_the_report_rate() {
        // Bluetooth と、それより速く届く場合(サンプルが前のレポートと重なる)
        for interval in [15, 8] {
            let [x, y] = turn_per_second(0.0, -30.0, Duration::from_millis(interval));
            assert_close(x, 30.0, 0.05);
            assert_close(y, 0.0, 1e-9);
        }
    }

    #[test]
    fn turning_moves_horizontally_whatever_the_tilt() {
        for tilt in [45.0, 120.0, -150.0] {
            let [x, y] = turn_per_second(tilt, -30.0, Duration::from_millis(15));
            assert_close(x, 30.0, 0.05);
            assert_close(y, 0.0, 0.05);
        }
    }

    #[test]
    fn turn_is_spread_until_the_next_report() {
        let mut tracker = Tracker::default();
        let mut motion = Motion::default();
        let start = Instant::now();
        let interval = Duration::from_millis(15);
        let turning = report(Some([yaw(0.0, -30.0); 3]));
        tracker.update(&mut motion, &turning, start);
        tracker.update(&mut motion, &turning, start + interval);
        let pending = motion.turn[0];

        // 5ms 毎に取り出すと、次のレポートまでの 3 回で均等に流れる
        let tick = Duration::from_millis(5);
        let taken: Vec<f64> = (1..=3)
            .map(|i| motion.take_turn(start + interval + tick * i, tick.as_secs_f64())[0])
            .collect();
        for taken in &taken {
            assert_close(*taken, pending / 3.0, 1e-9);
        }
        // 次のレポートが遅れても、それ以上は動かさない
        let late = start + interval + Duration::from_millis(50);
        assert_close(motion.take_turn(late, tick.as_secs_f64())[0], 0.0, 1e-12);
        assert_eq!(motion.take_turn(late, tick.as_secs_f64()), [0.0, 0.0]);
    }

    #[test]
    fn sticks_go_neutral_when_the_reports_stop() {
        let mut motion = Motion::default();
        let start = Instant::now();
        assert_eq!(motion.sticks(start), [[0.0; 2]; 2]);
        Tracker::default().update(&mut motion, &report(None), start);
        assert_eq!(motion.sticks(start + MAX_INPUT_GAP)[1], [1.0, 0.0]);
        assert_eq!(
            motion.sticks(start + MAX_INPUT_GAP + Duration::from_millis(1)),
            [[0.0; 2]; 2]
        );
    }

    #[test]
    fn report_without_motion_does_not_turn() {
        let mut motion = Motion::default();
        Tracker::default().update(&mut motion, &report(None), Instant::now());
        assert_eq!(motion.turn, [0.0, 0.0]);
    }
}
