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

use slint::Model;

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

/// One event-loop frame: run `changed` handlers (e.g. chart resize) then draw.
fn render(window: &MinimalSoftwareWindow) -> slint::SharedPixelBuffer<Rgba8Pixel> {
    slint::platform::update_timers_and_animations();
    window.take_snapshot().unwrap()
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

fn charger(name: &str, address: &str) -> ChargerDevice {
    ChargerDevice {
        name: name.into(),
        address: address.into(),
        detail: "-61 dBm".into(),
    }
}

// Logical positions of the single-row header controls in the 1120x760 window.
const SCAN: (f32, f32) = (815.0, 38.0);
const CONNECT: (f32, f32) = (884.0, 38.0);
const ENTRY_TOGGLE: (f32, f32) = (978.0, 38.0);

#[test]
fn dark_header_controls_are_visible_and_connect_selected_or_manual_address() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).unwrap();
    let ui = create_window(&AppConfig::default()).unwrap();
    ui.show().unwrap();

    set_device_list(
        &ui,
        vec![
            charger("#Charger-51F8", "DC:23:53:0C:F8:51"),
            charger("#Charger-0001", "AA:BB:CC:DD:EE:01"),
        ],
    );
    assert_eq!(
        ui.get_device_labels().row_data(0).unwrap(),
        "#Charger-51F8  ·  DC:23:53:0C:F8:51  (-61 dBm)"
    );

    let snapshot = render(&window);
    let stride = usize::try_from(snapshot.width()).unwrap();
    let pixels = snapshot.as_slice();
    assert_light_text(pixels, stride, 425..780, 16..60, "Device dropdown");
    assert_light_text(pixels, stride, 790..842, 16..60, "Scan button");
    assert_light_text(pixels, stride, 930..1028, 16..60, "Entry toggle");
    assert_light_text(pixels, stride, 28..260, 140..260, "Slot values");
    assert_eq!(ui.global::<Palette>().get_color_scheme(), ColorScheme::Dark);

    let scanned = Rc::new(Cell::new(false));
    ui.on_scan({
        let scanned = scanned.clone();
        move || scanned.set(true)
    });
    let connected_address = Rc::new(RefCell::new(Vec::<slint::SharedString>::new()));
    ui.on_connect({
        let connected_address = connected_address.clone();
        move |address| connected_address.borrow_mut().push(address)
    });

    click(ui.window(), SCAN.0, SCAN.1);
    assert!(scanned.get());

    ui.set_selected_device(1);
    click(ui.window(), CONNECT.0, CONNECT.1);
    assert_eq!(*connected_address.borrow(), ["AA:BB:CC:DD:EE:01"]);

    click(ui.window(), ENTRY_TOGGLE.0, ENTRY_TOGGLE.1);
    assert!(ui.get_manual_entry());
    click(ui.window(), CONNECT.0, CONNECT.1);
    assert_eq!(
        connected_address.borrow().len(),
        1,
        "empty manual address must not connect"
    );
    ui.set_address("11:22:33:44:55:66".into());
    click(ui.window(), CONNECT.0, CONNECT.1);
    assert_eq!(
        connected_address.borrow().last().unwrap(),
        "11:22:33:44:55:66"
    );
    let snapshot = render(&window);
    assert_light_text(
        snapshot.as_slice(),
        stride,
        425..780,
        16..60,
        "Manual address field",
    );

    click(ui.window(), ENTRY_TOGGLE.0, ENTRY_TOGGLE.1);
    assert!(!ui.get_manual_entry());

    ui.set_debug_visible(true);
    let snapshot = render(&window);
    assert_light_text(
        snapshot.as_slice(),
        stride,
        900..1104,
        550..630,
        "Debug panel controls",
    );
    narrow_window_collapses_slots_into_tabs(&window, &ui);
    history_chart_plots_and_toggles_slots(&window, &ui);
    ui.hide().unwrap();

    remembered_charger_is_listed_by_name_and_saved();
}

fn narrow_window_collapses_slots_into_tabs(window: &Rc<MinimalSoftwareWindow>, ui: &MainWindow) {
    ui.set_debug_visible(false);
    let mut slots = Slot::ALL.map(empty_slot_info).to_vec();
    slots[1].led = "green".into();
    ui.set_slots(ModelRc::new(VecModel::from(slots)));
    ui.window().set_size(slint::PhysicalSize::new(600, 640));
    assert!(ui.get_compact());

    let snapshot = render(window);
    let stride = usize::try_from(snapshot.width()).unwrap();
    let pixels = snapshot.as_slice();
    assert_light_text(pixels, stride, 16..584, 64..106, "Compact device picker");
    assert_light_text(pixels, stride, 16..584, 160..350, "Active slot card");
    let green_led = (113..147)
        .flat_map(|y| (160..290).map(move |x| pixels[y * stride + x]))
        .any(|pixel| pixel.g > 140 && pixel.r < 100 && pixel.b < 140);
    assert!(green_led, "slot tabs must show each slot's LED");

    // Fourth tab of four equal tabs spanning x 16..584.
    click(ui.window(), 515.0, 130.0);
    assert_eq!(ui.get_active_slot(), 3);

    // A status poll landing between press and release must not swallow the click.
    let position = slint::LogicalPosition::new(375.0, 130.0);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    let mut updated = empty_slot_info(Slot::Three);
    updated.led = "red".into();
    set_slot_row(ui, 2, updated);
    render(window);
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    assert_eq!(ui.get_active_slot(), 2);
    assert_eq!(ui.get_slots().row_data(2).unwrap().led, "red");

    ui.window().set_size(slint::PhysicalSize::new(1120, 760));
    assert!(!ui.get_compact());
}

fn is_slot_two_orange(pixel: Rgba8Pixel) -> bool {
    pixel.r > 200 && (120..190).contains(&pixel.g) && pixel.b < 80
}

fn count_orange(window: &MinimalSoftwareWindow, x: std::ops::Range<usize>) -> usize {
    let snapshot = render(window);
    let stride = usize::try_from(snapshot.width()).unwrap();
    let pixels = snapshot.as_slice();
    // Plot area only, excluding the legend swatches below it.
    (340..670)
        .flat_map(|y| x.clone().map(move |column| pixels[y * stride + column]))
        .filter(|&pixel| is_slot_two_orange(pixel))
        .count()
}

fn history_chart_plots_and_toggles_slots(window: &Rc<MinimalSoftwareWindow>, ui: &MainWindow) {
    use mc5000_protocol::{BatteryType, ErrorCode, ModeCode, StatusCode};
    ui.set_debug_visible(false);
    let origin = Instant::now()
        .checked_sub(Duration::from_secs(600))
        .unwrap();
    HISTORY.with_borrow_mut(|history| *history = History::new(origin));
    for second in 0..600u16 {
        let status = SlotStatus {
            leading_byte: 0,
            current_ma: 1000,
            voltage_mv: 1200 + second / 2,
            temperature_milli_celsius: 25_000,
            capacity_mah: second,
            elapsed_seconds: u32::from(second),
            resistance_raw: 30,
            status: StatusCode(1),
            mode: ModeCode(0),
            error: ErrorCode(0),
            battery_type: BatteryType(0),
        };
        let at = origin + Duration::from_secs(u64::from(second));
        HISTORY.with_borrow_mut(|history| history.record(Slot::Two, &status, at));
    }
    render(window);
    refresh_chart(ui);
    assert!(!ui.get_chart().empty);
    assert!(
        count_orange(window, 100..400) > 20,
        "chart starts at the left"
    );
    assert!(count_orange(window, 800..1090) > 20, "chart reaches 'now'");

    // Metric pills sit in the chart header: Voltage, Current, ...
    click(ui.window(), 129.0, 306.0);
    assert_eq!(ui.get_chart_metric(), 1);

    // The coloured bar on a slot card toggles that slot's line too.
    click(ui.window(), 304.0, 100.0);
    assert_eq!(chart_hidden(ui), [false, true, false, false]);
    assert_eq!(count_orange(window, 100..1090), 0, "hidden slot not drawn");
    click(ui.window(), 304.0, 100.0);
    assert_eq!(chart_hidden(ui), [false; 4]);
    assert!(count_orange(window, 100..1090) > 20, "slot shown again");

    // Clicking a legend entry hides that slot's series.
    click(ui.window(), LEGEND_SLOT_2.0, LEGEND_SLOT_2.1);
    assert_eq!(chart_hidden(ui), [false, true, false, false]);
    assert_eq!(count_orange(window, 100..1090), 0, "hidden slot not drawn");
    click(ui.window(), LEGEND_SLOT_2.0, LEGEND_SLOT_2.1);
    assert_eq!(chart_hidden(ui), [false; 4]);

    chart_zoom_and_time_window(window, ui);
    hovering_a_line_shows_its_value(window, ui);

    click(ui.window(), 1066.0, 309.0);
    assert!(ui.get_chart().empty, "Clear empties the chart");
    // With nothing captured, Export says so instead of opening a save dialog.
    click(ui.window(), EXPORT.0, EXPORT.1);
    assert_eq!(ui.get_status_line(), "No captured data to export yet");
}

// Chart header controls in the 1120x760 window.
const ZOOM_IN: (f32, f32) = (885.0, 306.0);
const AUTO_FIT: (f32, f32) = (932.0, 306.0);
const AVERAGE: (f32, f32) = (721.0, 306.0);
const SPLINE: (f32, f32) = (788.0, 306.0);
const EXPORT: (f32, f32) = (1003.0, 309.0);
// Legend and time window presets below the plot.
const LEGEND_SLOT_2: (f32, f32) = (570.0, 690.0);
const ALL_HISTORY: (f32, f32) = (392.0, 719.0);
const ONE_MINUTE: (f32, f32) = (442.0, 719.0);

fn hovering_a_line_shows_its_value(window: &MinimalSoftwareWindow, ui: &MainWindow) {
    // Find where slot 2's line crosses the middle of the plot.
    let snapshot = render(window);
    let stride = usize::try_from(snapshot.width()).unwrap();
    let line_y = (340..670)
        .find(|&y| is_slot_two_orange(snapshot.as_slice()[y * stride + 600]))
        .expect("slot 2 line crosses x = 600");
    #[allow(clippy::cast_precision_loss)]
    let on_line = slint::LogicalPosition::new(600.0, line_y as f32);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: on_line });
    render(window);
    let probe = ui.get_chart_probe();
    assert!(probe.visible, "hovering the line shows a tooltip");
    assert_eq!(probe.slot, 1);
    assert!(probe.value.ends_with(" V") && probe.value.starts_with("1."));
    assert_eq!(probe.time.len(), "12:34:56".len());

    let away = slint::LogicalPosition::new(600.0, on_line.y + 120.0);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: away });
    render(window);
    assert!(!ui.get_chart_probe().visible, "no tooltip away from lines");
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: on_line });
    render(window);
    assert!(ui.get_chart_probe().visible);
    ui.window().dispatch_event(WindowEvent::PointerExited);
    render(window);
    assert!(!ui.get_chart_probe().visible, "tooltip hides on leaving");
}

fn shown_range() -> (f64, f64) {
    CHART.with(Cell::get).shown
}

fn chart_zoom_and_time_window(window: &MinimalSoftwareWindow, ui: &MainWindow) {
    // Back to voltage: 1.2 V .. 1.5 V over the last ten minutes.
    click(ui.window(), 58.0, 306.0);
    render(window);
    let (low, high) = shown_range();
    assert!(!ui.get_chart_zoomed());

    click(ui.window(), ZOOM_IN.0, ZOOM_IN.1);
    assert!(ui.get_chart_zoomed(), "+ zooms the y axis in");
    let (zoomed_low, zoomed_high) = shown_range();
    assert!((zoomed_high - zoomed_low) < (high - low) * 0.9);
    click(ui.window(), AUTO_FIT.0, AUTO_FIT.1);
    assert!(!ui.get_chart_zoomed(), "Auto fits the data again");
    assert_eq!(shown_range(), (low, high));

    // Wheel over the plot zooms; dragging down pans the view to higher values.
    let plot = slint::LogicalPosition::new(600.0, 500.0);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position: plot });
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: plot,
        delta_x: 0.0,
        delta_y: 30.0,
    });
    assert!(ui.get_chart_zoomed(), "wheel zooms");
    let before = shown_range();
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: plot,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(600.0, 560.0),
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: slint::LogicalPosition::new(600.0, 560.0),
        button: PointerEventButton::Left,
    });
    assert!(shown_range().0 > before.0, "dragging down pans upwards");
    click(ui.window(), AUTO_FIT.0, AUTO_FIT.1);

    // Smoothing modes are exclusive; clicking the active one turns it off.
    click(ui.window(), AVERAGE.0, AVERAGE.1);
    assert_eq!(ui.get_chart_smoothing(), 1, "Average smooths");
    click(ui.window(), SPLINE.0, SPLINE.1);
    assert_eq!(ui.get_chart_smoothing(), 2, "Spline replaces Average");
    assert!(!ui.get_chart().empty);
    click(ui.window(), SPLINE.0, SPLINE.1);
    assert_eq!(ui.get_chart_smoothing(), 0);

    // A one-minute window shows only the last minute of the ten recorded.
    assert_eq!(ui.get_chart_windows().row_data(1).unwrap(), "1 min");
    click(ui.window(), ONE_MINUTE.0, ONE_MINUTE.1);
    assert_eq!(ui.get_chart_window(), 1);
    assert_eq!(ui.get_chart().x_start, "−60 s");
    assert!(shown_range().0 > low + (high - low) * 0.5);
    click(ui.window(), ALL_HISTORY.0, ALL_HISTORY.1);
    assert_eq!(ui.get_chart_window(), 0);
    assert_eq!(ui.get_chart().x_start, "−10 min");
}

fn remembered_charger_is_listed_by_name_and_saved() {
    let remembered = create_window(&AppConfig {
        address: "AA:BB:CC:DD:EE:01".to_owned(),
        device_name: "#Charger-0001".to_owned(),
        ..AppConfig::default()
    })
    .unwrap();
    assert_eq!(
        remembered.get_device_labels().row_data(0).unwrap(),
        "#Charger-0001  ·  AA:BB:CC:DD:EE:01  (last connected)"
    );
    let unnamed = create_window(&AppConfig {
        address: "AA:BB:CC:DD:EE:02".to_owned(),
        ..AppConfig::default()
    })
    .unwrap();
    assert_eq!(
        unnamed.get_device_labels().row_data(0).unwrap(),
        "MC5000  ·  AA:BB:CC:DD:EE:02  (last connected)"
    );
    set_device_list(
        &remembered,
        vec![
            charger("#Charger-51F8", "DC:23:53:0C:F8:51"),
            charger("#Charger-0001", "aa:bb:cc:dd:ee:01"),
        ],
    );
    assert_eq!(
        remembered.get_selected_device(),
        1,
        "scan results keep the last connected charger selected"
    );

    let mut config = AppConfig::default();
    remembered.set_address("DC:23:53:0C:F8:51".into());
    remember_device(&remembered, &mut config);
    assert_eq!(config.address, "DC:23:53:0C:F8:51");
    assert_eq!(config.device_name, "#Charger-51F8");
    remembered.set_address("11:22:33:44:55:66".into());
    remember_device(&remembered, &mut config);
    assert_eq!(config.address, "11:22:33:44:55:66");
    assert!(
        config.device_name.is_empty(),
        "an unscanned manual address must not inherit the previous name"
    );
}
