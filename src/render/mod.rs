use std::sync::mpsc;
use std::time::{Duration, Instant};
use winit::{
    event::{DeviceEvent, ElementState, Event, KeyboardInput, VirtualKeyCode, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::Window,
};

mod buffer;
mod camera;
mod d2;
mod d3;
mod ir_compute;
mod parameters;
mod texture;
mod uniforms;

pub fn create_render_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    color_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    vertex_descs: &[wgpu::VertexBufferDescriptor],
    vs_spv: &[u32],
    fs_spv: &[u32],
    sample_count: u32,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        layout: &layout,
        vertex_stage: wgpu::ProgrammableStageDescriptor {
            module: &device.create_shader_module(vs_spv),
            entry_point: "main",
        },
        fragment_stage: Some(wgpu::ProgrammableStageDescriptor {
            module: &device.create_shader_module(fs_spv),
            entry_point: "main",
        }),
        rasterization_state: Some(wgpu::RasterizationStateDescriptor {
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: wgpu::CullMode::None,
            depth_bias: 0,
            depth_bias_slope_scale: 0.0,
            depth_bias_clamp: 0.0,
        }),
        primitive_topology: wgpu::PrimitiveTopology::TriangleList,
        color_states: &[wgpu::ColorStateDescriptor {
            format: color_format,
            color_blend: wgpu::BlendDescriptor::REPLACE,
            alpha_blend: wgpu::BlendDescriptor::REPLACE,
            write_mask: wgpu::ColorWrite::ALL,
        }],
        depth_stencil_state: depth_format.map(|format| wgpu::DepthStencilStateDescriptor {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil_front: wgpu::StencilStateFaceDescriptor::IGNORE,
            stencil_back: wgpu::StencilStateFaceDescriptor::IGNORE,
            stencil_read_mask: 0,
            stencil_write_mask: 0,
        }),
        sample_count,
        sample_mask: !0,
        alpha_to_coverage_enabled: false,
        vertex_state: wgpu::VertexStateDescriptor {
            index_format: wgpu::IndexFormat::Uint32,
            vertex_buffers: vertex_descs,
        },
    })
}

fn create_multisampled_framebuffer(
    device: &wgpu::Device,
    sc_desc: &wgpu::SwapChainDescriptor,
    sample_count: u32,
) -> wgpu::TextureView {
    let multisampled_texture_extent = wgpu::Extent3d {
        width: sc_desc.width,
        height: sc_desc.height,
        depth: 1,
    };
    let multisampled_frame_descriptor = &wgpu::TextureDescriptor {
        size: multisampled_texture_extent,
        mip_level_count: 1,
        sample_count: sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: sc_desc.format,
        usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
        label: None,
    };

    device
        .create_texture(multisampled_frame_descriptor)
        .create_default_view()
}

struct GUI {
    surface: wgpu::Surface,
    device: wgpu::Device,
    queue: wgpu::Queue,
    sc_desc: wgpu::SwapChainDescriptor,
    swap_chain: wgpu::SwapChain,
    sample_count: u32,
    multisampled_framebuffer: wgpu::TextureView,
    uniforms: uniforms::UniformHandler,
    camera: camera::Camera,
    compute: ir_compute::IRCompute,
    render_d2: d2::D2,
    render_d3: d3::D3,
}

impl GUI {
    async fn new(window: &Window) -> Self {
        let sample_count = 16;
        let size = window.inner_size();
        let instance = wgpu::Instance::new();
        let surface = unsafe { instance.create_surface(window) };

        let adapter = instance
            .request_adapter(
                &wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::Default,
                    compatible_surface: Some(&surface),
                },
                wgpu::BackendBit::VULKAN,
            )
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    extensions: wgpu::Extensions {
                        anisotropic_filtering: false,
                    },
                    limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .unwrap();

        let sc_desc = wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
        };
        let swap_chain = device.create_swap_chain(&surface, &sc_desc);

        let camera = camera::Camera::new(&sc_desc);
        let mut uniforms = uniforms::UniformHandler::new(&device);
        uniforms.update_view_proj(&camera);

        let multisampled_framebuffer =
            create_multisampled_framebuffer(&device, &sc_desc, sample_count);

        let compute = ir_compute::IRCompute::new(&device, uniforms.bind_group_layout());
        let render_d2 = d2::D2::new(&device, &compute.texture_binding_layout, sample_count);
        let render_d3 = d3::D3::new(&device, &uniforms, &sc_desc, sample_count);

        Self {
            surface,
            device,
            queue,
            sc_desc,
            swap_chain,
            sample_count,
            multisampled_framebuffer,
            uniforms,
            camera,
            compute,
            render_d2,
            render_d3,
        }
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.sc_desc.width = new_size.width;
        self.sc_desc.height = new_size.height;
        self.swap_chain = self.device.create_swap_chain(&self.surface, &self.sc_desc);
        self.multisampled_framebuffer =
            create_multisampled_framebuffer(&self.device, &self.sc_desc, self.sample_count);
        self.render_d3
            .resize(&self.device, &self.sc_desc, self.sample_count);
        self.camera
            .update_aspect(self.sc_desc.width, self.sc_desc.height);
    }

    // input() won't deal with GPU code, so it can be synchronous
    fn input(&mut self, event: &WindowEvent) -> bool {
        self.camera.input(event)
    }

    fn update(&mut self, dt: Duration) {
        self.camera.update(dt);
        self.uniforms.update_view_proj(&self.camera);
    }

    fn push_ir_data(&mut self, image: image::GrayImage) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("IR compute shader"),
            });
        self.uniforms.upload(&self.device, &mut encoder);
        self.compute.push_ir_data(
            &self.device,
            &mut encoder,
            &self.uniforms.bind_group(),
            image,
        );
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn render(&mut self) {
        let frame = self
            .swap_chain
            .get_next_texture()
            .expect("Timeout when acquiring next swap chain texture");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Rendering"),
            });

        self.uniforms.upload(&self.device, &mut encoder);

        {
            let mut rpass3d = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[self.color_attachment(&frame, wgpu::LoadOp::Clear)],
                depth_stencil_attachment: Some(self.render_d3.depth_stencil_attachement()),
            });
            if self.compute.texture_binding.is_some() {
                self.render_d3
                    .render(&mut rpass3d, &self.compute, &self.uniforms);
            }
        }

        if let Some(ref texture) = self.compute.texture_binding {
            let mut rpass2d = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[self.color_attachment(&frame, wgpu::LoadOp::Load)],
                depth_stencil_attachment: None,
            });
            self.render_d2.render(&mut rpass2d, texture);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn color_attachment<'a>(
        &'a self,
        frame: &'a wgpu::SwapChainOutput,
        load_op: wgpu::LoadOp,
    ) -> wgpu::RenderPassColorAttachmentDescriptor {
        if self.sample_count == 1 {
            wgpu::RenderPassColorAttachmentDescriptor {
                attachment: &frame.view,
                resolve_target: None,
                load_op,
                store_op: wgpu::StoreOp::Store,
                clear_color: wgpu::Color::BLUE,
            }
        } else {
            wgpu::RenderPassColorAttachmentDescriptor {
                attachment: &self.multisampled_framebuffer,
                resolve_target: Some(&frame.view),
                load_op,
                store_op: wgpu::StoreOp::Store,
                clear_color: wgpu::Color::BLUE,
            }
        }
    }
}

pub async fn run(
    event_loop: EventLoop<JoyconData>,
    window: Window,
    thread_contact: mpsc::Sender<JoyconCmd>,
    _thread_handle: std::thread::JoinHandle<anyhow::Result<()>>,
) -> ! {
    let mut gui = GUI::new(&window).await;
    window.set_maximized(true);

    //let mut thread_handle = Some(thread_handle);

    let mut parameters = parameters::Parameters::new();
    let mut hidden = false;

    fn set_grabbed(window: &Window, grabbed: bool) -> bool {
        window.set_cursor_grab(grabbed).unwrap();
        window.set_cursor_visible(!grabbed);
        grabbed
    };
    let mut grabbed = set_grabbed(&window, false);

    let mut last_tick = Instant::now();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Poll;
        match event {
            Event::MainEventsCleared => {
                if !hidden {
                    gui.update(last_tick.elapsed());
                }
                last_tick = Instant::now();
                window.request_redraw();
            }
            Event::RedrawRequested(_) => {
                if !hidden {
                    gui.render();
                }
            }
            Event::LoopDestroyed => {
                eprintln!("sending shutdown signal to thread");
                let _ = thread_contact.send(JoyconCmd::Stop);
                // TODO: join thread with timeout
                std::thread::sleep(Duration::from_millis(500));
                /*match thread_handle
                    .take()
                    .expect("thread already exited???")
                    .join()
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("Joycon thread exited with error: {:?}", e),
                    Err(_) => eprintln!("Joycon thread crashed"),
                }*/
            }
            Event::UserEvent(JoyconData::IRImage(image, position)) => {
                gui.push_ir_data(image);
                gui.uniforms.set_ir_rotation(position.rotation);
                window.request_redraw();
            }
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } if grabbed => {
                gui.camera.mouse_move(delta);
                window.request_redraw();
            }
            Event::WindowEvent {
                ref event,
                window_id,
            } if window_id == window.id() => {
                if gui.input(event) || parameters.input(event, &thread_contact) {
                    window.request_redraw();
                } else {
                    match event {
                        WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                        WindowEvent::KeyboardInput {
                            input:
                                KeyboardInput {
                                    state: ElementState::Pressed,
                                    virtual_keycode: Some(VirtualKeyCode::Escape),
                                    ..
                                },
                            ..
                        } => *control_flow = ControlFlow::Exit,
                        WindowEvent::KeyboardInput {
                            input:
                                KeyboardInput {
                                    state: ElementState::Pressed,
                                    virtual_keycode: Some(VirtualKeyCode::Space),
                                    ..
                                },
                            ..
                        } => {
                            grabbed = set_grabbed(&window, !grabbed);
                        }
                        WindowEvent::Resized(physical_size) => {
                            if physical_size.height != 0 && physical_size.height != 0 {
                                hidden = false;
                                gui.resize(*physical_size);
                            } else {
                                hidden = true;
                            }
                        }
                        WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                            if new_inner_size.height != 0 && new_inner_size.height != 0 {
                                hidden = false;
                                gui.resize(**new_inner_size);
                            } else {
                                hidden = true;
                            }
                        }
                        WindowEvent::Focused(false) => {
                            grabbed = set_grabbed(&window, false);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    });
}

pub enum JoyconData {
    IRImage(image::GrayImage, crate::imu_handler::Position),
}

pub enum JoyconCmd {
    Stop,
    SetResolution(joycon_sys::mcu::ir::Resolution),
    SetRegister(joycon_sys::mcu::ir::Register),
}
