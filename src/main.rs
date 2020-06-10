use hidapi::HidApi;
use iced_winit::winit::{
    self,
    event_loop::{EventLoop, EventLoopProxy},
};
use joycon::{
    joycon_sys::{light, mcu::ir::*, NINTENDO_VENDOR_ID},
    *,
};
use render::*;
use std::f32::consts::PI;
use std::sync::mpsc;

mod mouse;
mod render;

fn main() {
    env_logger::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    std::panic::set_hook(Box::new(|x| {
        println!("{}", x);
        std::thread::sleep(std::time::Duration::from_secs(5));
    }));

    let event_loop = EventLoop::with_user_event();
    let window = winit::window::WindowBuilder::new()
        .with_maximized(true)
        .with_title("Joy")
        .build(&event_loop)
        .unwrap();
    let proxy = event_loop.create_proxy();
    let (thread_contact, recv) = mpsc::channel();
    let thread_handle = std::thread::spawn(|| real_main(proxy, recv));

    render::run(event_loop, window, thread_contact, thread_handle);
}

fn real_main(
    proxy: EventLoopProxy<JoyconData>,
    recv: mpsc::Receiver<JoyconCmd>,
) -> anyhow::Result<()> {
    let mut image = ::image::GrayImage::new(240, 320);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = [
            (((x as f32 / 240. * PI).sin() * (y as f32 / 320. * PI).sin()).powf(10.) * 255.) as u8,
        ]
        .into();
    }
    assert!(proxy
        .send_event(JoyconData::IRImage(image, Position::default()))
        .is_ok());
    let mut api = HidApi::new()?;
    loop {
        api.refresh_devices()?;
        if let Some(device_info) = api
            .device_list()
            .filter(|x| x.vendor_id() == NINTENDO_VENDOR_ID)
            .next()
        {
            let device = device_info.open_device(&api)?;
            match hid_main(device, device_info, proxy.clone(), &recv) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(e) => println!("Joycon error: {}", e),
            }
        } else {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

fn hid_main(
    device: hidapi::HidDevice,
    device_info: &hidapi::DeviceInfo,
    proxy: EventLoopProxy<JoyconData>,
    recv: &mpsc::Receiver<JoyconCmd>,
) -> anyhow::Result<bool> {
    let mut _mouse = mouse::Mouse::new();

    let mut device = JoyCon::new(device, device_info.clone())?;
    println!("new dev: {:?}", device.get_dev_info()?);

    println!("Calibrating...");
    device.enable_imu()?;
    device.load_calibration()?;
    println!("Running...");

    dbg!(device.set_home_light(light::HomeLight::new(
        0x8,
        0x2,
        0x0,
        &[(0xf, 0xf, 0), (0x2, 0xf, 0)],
    ))?);

    device.set_player_light(light::PlayerLights::new(
        true, false, false, true, false, true, true, false,
    ))?;

    device.enable_ir(Resolution::R160x120)?;

    let mut last_position = Position::default();
    let mut last_battery_level = dbg!(device.tick()?.info.battery_level());

    'main_loop: loop {
        let report = device.tick()?;

        if let Some(image) = report.image {
            if let Err(_) = proxy.send_event(JoyconData::IRImage(image, last_position)) {
                dbg!("shutdown ");
                break 'main_loop;
            }
            last_position = report.position;
        }

        let battery_level = report.info.battery_level();
        if battery_level != last_battery_level {
            println!("Battery level down to {:?}", battery_level);
            last_battery_level = battery_level;
        }

        'recv_loop: loop {
            match recv.try_recv() {
                Ok(JoyconCmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("shutting down thread");
                    break 'main_loop;
                }
                Ok(JoyconCmd::SetResolution(resolution)) => {
                    dbg!(device.change_ir_resolution(resolution)?);
                }
                Ok(JoyconCmd::SetRegister(register)) => {
                    assert!(!register.same_address(Register::resolution(Resolution::R320x240)));
                    dbg!(device.set_ir_registers(&[register, Register::finish()])?);
                }
                Ok(JoyconCmd::SetRegisters([r1, r2])) => {
                    assert!(!r1.same_address(Register::resolution(Resolution::R320x240)));
                    assert!(!r2.same_address(Register::resolution(Resolution::R320x240)));
                    dbg!(device.set_ir_registers(&[r1, r2, Register::finish()])?);
                }
                Err(mpsc::TryRecvError::Empty) => break 'recv_loop,
            }
        }
    }

    dbg!(device.tick()?.info.battery_level());

    dbg!(device.disable_mcu()?);

    dbg!(device.set_player_light(light::PlayerLights::new(
        true, false, false, true, false, false, false, false,
    ))?);
    dbg!(device.set_home_light(light::HomeLight::new(0x8, 0x4, 0x0, &[]))?);

    Ok(false)
}
