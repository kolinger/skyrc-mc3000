use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use slint::{
    Rgba8Pixel,
    language::ColorScheme,
    platform::{
        Platform, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};

use super::*;

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn assert_light_text(
    pixels: &[Rgba8Pixel],
    stride: usize,
    x: std::ops::Range<usize>,
    y: std::ops::Range<usize>,
    label: &str,
) {
    let dark_pixels = y
        .flat_map(|row| x.clone().map(move |column| pixels[row * stride + column]))
        .filter(|pixel| pixel.r > 150 && pixel.g > 150 && pixel.b > 150)
        .count();
    assert!(dark_pixels > 20, "{label} must have readable light text");
}

fn click(window: &slint::Window, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn dark_surfaces_have_visible_controls_and_slot_values() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).unwrap();
    let ui = create_window(&AppConfig::default()).unwrap();

    ui.show().unwrap();
    let snapshot = window.take_snapshot().unwrap();
    let stride = usize::try_from(snapshot.width()).unwrap();
    let pixels = snapshot.as_slice();

    assert_light_text(pixels, stride, 985..1035, 16..80, "Scan button");
    assert_light_text(pixels, stride, 1034..1104, 80..125, "Connect button");
    assert_light_text(pixels, stride, 16..250, 80..125, "Address placeholder");
    assert_light_text(pixels, stride, 150..260, 320..740, "Slot values");
    assert_eq!(ui.global::<Palette>().get_color_scheme(), ColorScheme::Dark);

    let scanned = Rc::new(Cell::new(false));
    ui.on_scan({
        let scanned = scanned.clone();
        move || scanned.set(true)
    });
    let connected_address = Rc::new(RefCell::new(slint::SharedString::default()));
    ui.on_connect({
        let connected_address = connected_address.clone();
        move |address| {
            *connected_address.borrow_mut() = address;
        }
    });
    ui.set_address("AA:BB:CC:DD:EE:FF".into());
    click(ui.window(), 1010.0, 42.0);
    click(ui.window(), 1067.0, 96.0);
    assert!(scanned.get());
    assert_eq!(*connected_address.borrow(), "AA:BB:CC:DD:EE:FF");

    ui.set_debug_visible(true);
    let snapshot = window.take_snapshot().unwrap();
    assert_light_text(
        snapshot.as_slice(),
        usize::try_from(snapshot.width()).unwrap(),
        900..1104,
        490..590,
        "Debug panel controls",
    );
    ui.hide().unwrap();
}
