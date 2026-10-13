//! Exercise the real DX12 swap chain, including resize and surface recreation.
#![cfg(windows)]

use iced::wgpu;
use wgpu::rwh::{RawDisplayHandle, Win32WindowHandle, WindowsDisplayHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

struct TestWindow(HWND);

impl TestWindow {
    fn new() -> Self {
        // SAFETY: STATIC is a system window class; the window stays on this thread.
        Self(unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("Legato resize test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        })
    }

    fn resize(&self, width: u32, height: u32) {
        // SAFETY: this thread owns the live window; only its size changes.
        unsafe {
            SetWindowPos(
                self.0,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
            )
            .unwrap();
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: the surface has already been dropped, on the window's owning thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[test]
#[ignore = "requires a Windows desktop and DX12 (WARP is sufficient)"]
fn gpu_renders_resized_and_recreated_windows() {
    iced::futures::executor::block_on(async {
        // Cover the app's default and the supported resize-diagnostic alternative.
        for present_mode in [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo] {
            let window = TestWindow::new();
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::DX12,
                ..Default::default()
            });
            // Recreate as well as resize, as when recovering a lost surface.
            for _ in 0..2 {
                // SAFETY: the HWND outlives the surface and is owned by this thread.
                let surface = unsafe {
                    instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                        raw_display_handle: RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
                        raw_window_handle: Win32WindowHandle::new(
                            std::num::NonZeroIsize::new(window.0.0 as isize).unwrap(),
                        )
                        .into(),
                    })
                }
                .unwrap();
                let adapter = instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        compatible_surface: Some(&surface),
                        force_fallback_adapter: true,
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
                let mut config = surface.get_default_config(&adapter, 320, 240).unwrap();
                config.present_mode = present_mode;
                config.desired_maximum_frame_latency = 1; // Matches Iced's compositor.

                for (width, height) in [(320, 240), (640, 480), (300, 220)] {
                    window.resize(width, height);
                    config.width = width;
                    config.height = height;
                    surface.configure(&device, &config);

                    let frame = surface.get_current_texture().unwrap();
                    assert_eq!(
                        (frame.texture.width(), frame.texture.height()),
                        (width, height),
                        "{present_mode:?} after resize",
                    );
                    let view = frame.texture.create_view(&Default::default());
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            ..Default::default()
                        });
                    }
                    queue.submit([encoder.finish()]);
                    frame.present();
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                }
            }
        }
    });
}
