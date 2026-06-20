use alloc::sync::Arc;
use core::{
    fmt::Debug,
    fmt::Formatter,
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{fmt, sync::Mutex, time::Duration, vec};

use fast_image_resize::ResizeAlg;
use fast_image_resize::{PixelType, ResizeOptions, SrcCropping};
use softbuffer::{Context, Surface};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, KeyEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window as WinitWindow, WindowId},
};

use wie_backend::{Screen, canvas::Image};

#[derive(Debug)]
pub enum WindowInternalEvent {
    RequestRedraw,
    Paint,
    Quit,
}

pub enum WindowCallbackEvent {
    Update,
    Redraw,
    Keydown(PhysicalKey),
    Keyup(PhysicalKey),
}

#[derive(Clone)]
pub struct WindowHandle {
    width: u32,
    height: u32,
    event_loop_proxy: EventLoopProxy<WindowInternalEvent>,
    latest_frame: Arc<Mutex<Option<Vec<u32>>>>,
    paint_event_pending: Arc<AtomicBool>,
    redraw_event_pending: Arc<AtomicBool>,
}

impl WindowHandle {
    pub fn send_quit_event(&self) {
        let _ = self.send_event(WindowInternalEvent::Quit);
    }

    fn send_event(&self, event: WindowInternalEvent) -> wie_util::Result<()> {
        self.event_loop_proxy
            .send_event(event)
            .map_err(|_| wie_util::WieError::FatalError("Window event loop is closed".into()))
    }
}

impl Screen for WindowHandle {
    fn request_redraw(&self) -> wie_util::Result<()> {
        if self.redraw_event_pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        if let Err(error) = self.send_event(WindowInternalEvent::RequestRedraw) {
            self.redraw_event_pending.store(false, Ordering::Release);
            return Err(error);
        }

        Ok(())
    }

    fn paint(&self, image: &dyn Image) {
        *self.latest_frame.lock().unwrap() = Some(image.argb8888());
        if self.paint_event_pending.swap(true, Ordering::AcqRel) {
            return;
        }

        // The emulator thread can finish its current frame after the user has
        // already closed the window. Dropping that final frame is expected;
        // panicking here would turn an orderly shutdown into a worker failure.
        if self.send_event(WindowInternalEvent::Paint).is_err() {
            self.paint_event_pending.store(false, Ordering::Release);
        }
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }
}

pub struct WindowImpl {
    width: u32,
    height: u32,
    event_loop: EventLoop<WindowInternalEvent>,
    latest_frame: Arc<Mutex<Option<Vec<u32>>>>,
    paint_event_pending: Arc<AtomicBool>,
    redraw_event_pending: Arc<AtomicBool>,
}

impl WindowImpl {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let event_loop = EventLoop::<WindowInternalEvent>::with_user_event().build()?;

        Ok(Self {
            width,
            height,
            event_loop,
            latest_frame: Arc::new(Mutex::new(None)),
            paint_event_pending: Arc::new(AtomicBool::new(false)),
            redraw_event_pending: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn handle(&self) -> WindowHandle {
        WindowHandle {
            width: self.width,
            height: self.height,
            event_loop_proxy: self.event_loop.create_proxy(),
            latest_frame: self.latest_frame.clone(),
            paint_event_pending: self.paint_event_pending.clone(),
            redraw_event_pending: self.redraw_event_pending.clone(),
        }
    }

    pub fn run<C>(self, callback: C) -> anyhow::Result<()>
    where
        C: FnMut(WindowCallbackEvent) -> wie_util::Result<()> + 'static,
    {
        self.event_loop.set_control_flow(ControlFlow::Wait);

        let orig_size = LogicalSize::new(self.width, self.height);
        let mut handler = ApplicationHandlerImpl {
            window_scale: 1,
            content_size: orig_size,
            scaled_size: orig_size.to_physical(1.0),
            window_size: Default::default(),
            scaler: Scaler::Native,
            scaled_image_buf: Default::default(),
            window: None,
            context: None,
            surface: None,
            callback: Box::new(callback),
            last_frame: vec![0u32; (self.width * self.height) as usize],
            latest_frame: self.latest_frame,
            paint_event_pending: self.paint_event_pending,
            redraw_event_pending: self.redraw_event_pending,
        };

        Ok(self.event_loop.run_app(&mut handler)?)
    }
}

enum Scaler {
    /// 1:1 native scaling.
    Native,
    /// Nearest-neighbor integer scaling.
    Nearest { scale: u32 },
    /// hq2x, hq3x, hq4x scaling.
    Hqx { scale: i8 },
    /// Lanczos3 scaling
    Lanczos3 { scale: f64, resizer: fast_image_resize::Resizer },
}

impl fmt::Display for Scaler {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Scaler::Native => f.write_str("Native")?,
            Scaler::Nearest { scale } => f.write_fmt(format_args!("Nearest({scale}x)"))?,
            Scaler::Hqx { scale } => f.write_fmt(format_args!("Hq{scale}x"))?,
            Scaler::Lanczos3 { scale, resizer: _ } => f.write_fmt(format_args!("Lanczos3({scale})"))?,
        }
        Ok(())
    }
}

fn scale_nearest_integer(dst: &mut [u32], src: &[u32], src_w: u32, src_h: u32, scale: u32) {
    let src_w = src_w as usize;
    let src_h = src_h as usize;
    let scale = scale as usize;
    let dst_w = src_w * scale;

    for y in 0..src_h {
        for sy in 0..scale {
            let dst_row = (y * scale + sy) * dst_w;
            let src_row = y * src_w;

            for x in 0..src_w {
                let px = src[src_row + x];
                let dst_x = x * scale;

                for sx in 0..scale {
                    dst[dst_row + dst_x + sx] = px;
                }
            }
        }
    }
}

impl Scaler {
    fn new(scale: u32) -> Scaler {
        let scale = scale.max(1);

        match scale {
            1 => Scaler::Native,
            _ => Scaler::Nearest { scale },
        }
    }

    #[allow(dead_code)]
    fn new_hqx(scale: f64) -> Scaler {
        match scale {
            _ if scale < 1.5 => Scaler::Native,
            _ if scale < 2.5 => Scaler::Hqx { scale: 2 },
            _ if scale < 3.5 => Scaler::Hqx { scale: 3 },
            _ => Scaler::Hqx { scale: 4 },
        }
    }

    #[allow(dead_code)]
    fn new_smooth(scale: f64) -> Scaler {
        match scale {
            _ if (scale - 1.0).abs() < 1e-3 => Scaler::Native,
            _ => Scaler::Lanczos3 {
                scale,
                resizer: fast_image_resize::Resizer::new(),
            },
        }
    }

    fn scale(&self) -> f64 {
        match self {
            Scaler::Native => 1.0,
            Scaler::Nearest { scale } => *scale as f64,
            Scaler::Hqx { scale } => *scale as f64,
            Scaler::Lanczos3 { scale, resizer: _ } => *scale,
        }
    }

    fn to_physical(&self, logical_size: LogicalSize<u32>) -> PhysicalSize<u32> {
        match self {
            Scaler::Native => PhysicalSize::new(logical_size.width, logical_size.height),
            Scaler::Nearest { scale } => PhysicalSize::new(logical_size.width * *scale, logical_size.height * *scale),
            Scaler::Hqx { scale } => PhysicalSize::new(logical_size.width * *scale as u32, logical_size.height * *scale as u32),
            Scaler::Lanczos3 { scale, resizer: _ } => PhysicalSize::new(
                (logical_size.width as f64 * *scale).floor() as u32,
                (logical_size.height as f64 * *scale).floor() as u32,
            ),
        }
    }

    fn scale_image(&mut self, dst: &mut Vec<u32>, src: &Vec<u32>, dst_size: PhysicalSize<u32>, src_size: LogicalSize<u32>) {
        match self {
            Scaler::Native => dst.copy_from_slice(src),

            Scaler::Nearest { scale } => {
                scale_nearest_integer(dst.as_mut_slice(), src.as_slice(), src_size.width, src_size.height, *scale);
            }

            Scaler::Hqx { scale } if *scale == 2 => hqx::hq2x(src.as_slice(), dst.as_mut_slice(), src_size.width as usize, src_size.height as usize),
            Scaler::Hqx { scale } if *scale == 3 => hqx::hq3x(src.as_slice(), dst.as_mut_slice(), src_size.width as usize, src_size.height as usize),
            Scaler::Hqx { scale } if *scale == 4 => hqx::hq4x(src.as_slice(), dst.as_mut_slice(), src_size.width as usize, src_size.height as usize),
            Scaler::Hqx { scale } => panic!("invalid hqx scale factor {scale}"),

            Scaler::Lanczos3 { scale: _, resizer } => {
                let (_, srcarr, _) = unsafe { src.align_to::<u8>() };
                let srcimg = fast_image_resize::images::ImageRef::new(src_size.width, src_size.height, srcarr, PixelType::U8x4).unwrap();
                let (_, dstarr, _) = unsafe { dst.as_mut_slice().align_to_mut::<u8>() };
                let mut dstimg = fast_image_resize::images::Image::from_slice_u8(dst_size.width, dst_size.height, dstarr, PixelType::U8x4).unwrap();
                resizer
                    .resize(
                        &srcimg,
                        &mut dstimg,
                        Some(&ResizeOptions {
                            #[cfg(debug_assertions)]
                            algorithm: ResizeAlg::Nearest,
                            #[cfg(not(debug_assertions))]
                            algorithm: ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3),
                            cropping: SrcCropping::None,
                            mul_div_alpha: false,
                        }),
                    )
                    .unwrap();
            }
        }
    }
}

pub struct ApplicationHandlerImpl<C>
where
    C: FnMut(WindowCallbackEvent) -> wie_util::Result<()> + 'static,
{
    window_scale: u32,
    /// Scaler config.
    scaler: Scaler,
    /// Temporary buffer for scaler.
    scaled_image_buf: Vec<u32>,

    /// content screen size.
    content_size: LogicalSize<u32>,
    /// Scaled screen size.
    /// Equals to orig_size * scale_factor.
    scaled_size: PhysicalSize<u32>,
    /// Size of the OS window.
    window_size: PhysicalSize<u32>,
    /// Last content screen image data.
    last_frame: Vec<u32>,

    window: Option<Arc<WinitWindow>>,
    context: Option<Context<Arc<WinitWindow>>>,
    surface: Option<Surface<Arc<WinitWindow>, Arc<WinitWindow>>>,
    callback: Box<C>,
    latest_frame: Arc<Mutex<Option<Vec<u32>>>>,
    paint_event_pending: Arc<AtomicBool>,
    redraw_event_pending: Arc<AtomicBool>,
}

impl<C> ApplicationHandlerImpl<C>
where
    C: FnMut(WindowCallbackEvent) -> wie_util::Result<()> + 'static,
{
    fn callback(&mut self, event: WindowCallbackEvent, event_loop: &ActiveEventLoop) {
        let result = (self.callback)(event);
        if let Err(x) = result {
            tracing::error!(target: "wie", "{x}");

            event_loop.exit();
        }
    }

    fn update_scale_factor(&mut self, scale: u32) {
        self.window_scale = scale.max(1);
        self.scaler = Scaler::new(self.window_scale);
        self.scaled_size = self.scaler.to_physical(self.content_size);
        self.scaled_image_buf = vec![0u32; self.scaled_size.width as usize * self.scaled_size.height as usize];
    }

    fn set_window_scale(&mut self, scale: u32) {
        self.update_scale_factor(scale);
        if let Some(window) = self.window.as_ref()
            && let Some(new_size) = window.request_inner_size(self.scaled_size)
        {
            self.window_size = new_size;
        }
        self.on_resize();
    }

    /// Updates the scaled content image surface's size.
    fn on_resize(&mut self) {
        tracing::info!(
            "on_resize scale={}, content={:?}, scaled={:?}, window={:?}",
            self.scaler.scale(),
            self.content_size,
            self.scaled_size,
            self.window_size
        );
        let surface = match self.surface.as_mut() {
            None => {
                self.surface = Some(Surface::new(self.context.as_ref().unwrap(), self.window.as_ref().unwrap().clone()).unwrap());
                self.surface.as_mut().unwrap()
            }
            Some(surface) => {
                let desired_len = self.scaled_size.width * self.scaled_size.height;
                if surface.buffer_mut().unwrap().len() == desired_len as usize {
                    self.paint_last_frame();
                    return;
                }
                surface
            }
        };

        surface
            .resize(
                NonZeroU32::new(self.scaled_size.width).unwrap(),
                NonZeroU32::new(self.scaled_size.height).unwrap(),
            )
            .unwrap();
        self.paint_last_frame();
    }

    /// Displays the last content frame to the window.
    fn paint_last_frame(&mut self) -> Option<()> {
        let data = &self.last_frame;
        let data_to_blit = if self.scaled_image_buf.len() == data.len() {
            data
        } else {
            self.scaler
                .scale_image(&mut self.scaled_image_buf, data, self.scaled_size, self.content_size);
            &self.scaled_image_buf
        };

        let mut win_buf = self.surface.as_mut().unwrap().buffer_mut().unwrap();
        if win_buf.len() == data_to_blit.len() {
            win_buf.copy_from_slice(data_to_blit);
        } else {
            tracing::warn!(
                "buffer size mismatch, skipping paint: {}, {} (content {:?}, scaled {:?}, win {:?})",
                win_buf.len(),
                data_to_blit.len(),
                self.content_size,
                self.scaled_size,
                self.window_size
            );
            return None;
        }
        win_buf.present().unwrap();
        Some(())
    }
}

impl<C> ApplicationHandler<WindowInternalEvent> for ApplicationHandlerImpl<C>
where
    C: FnMut(WindowCallbackEvent) -> wie_util::Result<()> + 'static,
{
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: StartCause) {
        self.callback(WindowCallbackEvent::Update, event_loop);
        event_loop.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(4)));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Initialize the window.
        let window_attributes = WinitWindow::default_attributes()
            .with_inner_size(self.content_size.to_physical::<u32>(1.0))
            .with_title("WIE");
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        let context = Context::new(window.clone()).unwrap();
        self.window = Some(window.clone());
        self.context = Some(context);
        self.window_size = window.inner_size();

        self.update_scale_factor(1);
        self.on_resize();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WindowInternalEvent) {
        match event {
            WindowInternalEvent::RequestRedraw => {
                self.redraw_event_pending.store(false, Ordering::Release);
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowInternalEvent::Paint => {
                self.paint_event_pending.store(false, Ordering::Release);
                let latest_frame = self.latest_frame.lock().unwrap().take();
                if let Some(data) = latest_frame {
                    self.last_frame = data;
                    self.paint_last_frame();
                }
            }
            WindowInternalEvent::Quit => {
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key,
                        state,
                        repeat: false,
                        ..
                    },
                ..
            } => {
                if let ElementState::Pressed = state
                    && let PhysicalKey::Code(code) = physical_key
                {
                    match code {
                        KeyCode::Equal | KeyCode::NumpadAdd => {
                            self.set_window_scale(self.window_scale + 1);
                            return;
                        }
                        KeyCode::Minus | KeyCode::NumpadSubtract => {
                            self.set_window_scale(self.window_scale.saturating_sub(1));
                            return;
                        }
                        _ => {}
                    }
                }

                match state {
                    ElementState::Pressed => {
                        self.callback(WindowCallbackEvent::Keydown(physical_key), event_loop);
                    }
                    ElementState::Released => {
                        self.callback(WindowCallbackEvent::Keyup(physical_key), event_loop);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.callback(WindowCallbackEvent::Redraw, event_loop);
            }
            WindowEvent::Resized(new_size) => {
                tracing::debug!("WindowResized {new_size:?}");
                self.window_size = new_size;
                self.on_resize();
            }
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let _ = inner_size_writer.request_inner_size(self.scaled_size);
                // Will receive WindowEvent::Resized soon, so no need to call self.on_resize().
            }
            _ => {}
        }
    }
}
