//! The compositor on a lost device (#243): renders return `Unsupported` (so callers use the CPU
//! compositor) instead of panicking with "Buffer 'pc_compose_uniforms' is invalid". Skips when no
//! GPU adapter exists.

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size};
use photocraft_gpu::{Compositor, DeviceHealth, Fault, render_to_vec};
use photocraft_raster::Surface;

/// Concurrent wgpu instances in one process segfault on some drivers (see `parity.rs`).
static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn device() -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skipping device-loss tests: no adapter ({e})");
            return None;
        }
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some((adapter, device, queue))
}

fn doc() -> Document {
    let mut d = Document::with_background("t", Size::new(600, 400), ColorMode::Rgb, SampleType::U8, Color::rgba(0.9, 0.9, 0.9, 1.0));
    let mut s = Surface::new(d.pixel_format());
    s.fill_rect(Rect::new(20, 20, 500, 300), &[0.8, 0.1, 0.1, 0.7]);
    d.layers.push(Layer::new("red", LayerContent::Raster(s)));
    d
}

#[test]
fn injected_fault_skips_the_gpu() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((adapter, device, queue)) = device() else { return };
    let Ok(mut comp) = Compositor::try_new_with_format(&device, Compositor::preferred_acc_format(&adapter)) else { return };
    let health = DeviceHealth::watch(&device);
    comp.set_health(health.clone());
    let d = doc();
    assert!(render_to_vec(&mut comp, &device, &queue, &d, d.bounds()).is_ok());
    assert!(comp.fault().is_none());
    health.mark(Fault::Lost("injected".into()));
    let e = render_to_vec(&mut comp, &device, &queue, &d, d.bounds()).unwrap_err();
    assert!(e.0.contains("GPU device lost: injected"), "{e:?}");
}

#[test]
fn destroyed_device_returns_an_error_not_a_panic() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((adapter, device, queue)) = device() else { return };
    let Ok(mut comp) = Compositor::try_new_with_format(&device, Compositor::preferred_acc_format(&adapter)) else { return };
    let health = DeviceHealth::watch(&device);
    comp.set_health(health.clone());
    let d = doc();
    assert!(render_to_vec(&mut comp, &device, &queue, &d, d.bounds()).is_ok());
    device.destroy();
    // The first render after the loss runs into it (uploads, uniforms, submit, readback);
    // every one after sees the flag and touches nothing.
    let mut d2 = d.clone();
    d2.layers[1].opacity = 0.4;
    for _ in 0..3 {
        assert!(render_to_vec(&mut comp, &device, &queue, &d2, d2.bounds()).is_err());
    }
    assert!(health.fault().is_some(), "loss not detected");
    assert!(health.errors() > 0 || health.fault().is_some_and(|f| f.is_lost()));
}

#[test]
fn health_wait_on_a_destroyed_device_is_safe() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((_adapter, device, _queue)) = device() else { return };
    let health = DeviceHealth::watch(&device);
    assert!(health.wait(&device, None));
    device.destroy();
    // Polling a destroyed device panics inside wgpu; the wait records a loss instead.
    health.wait(&device, None);
    health.poll(&device);
    assert!(!health.is_ok());
    assert!(!health.wait(&device, None));
}
