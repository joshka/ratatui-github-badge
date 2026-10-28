use badge_ui::{App, Key, Sensors};
use ratatui::{Terminal, backend::TestBackend};

fn sensors() -> Sensors {
    Sensors { accel: [0.1, 0.9, 0.2], imu_ok: true, battery_v: 3.9, light: 0.4, touch_ok: true, frame_ms: 40, heap_used: 40_000, heap_free: 120_000, ..Default::default() }
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(53, 24)).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer();
    (0..24).map(|y| (0..53).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect()
}

#[test]
fn every_page_and_subtab_renders() {
    let mut app = App::new();
    for _ in 0..30 {
        app.update(40, sensors());
    }
    for page in 0..7 {
        for _ in 0..4 {
            for _ in 0..20 {
                app.update(40, sensors());
                render(&mut app);
            }
            if std::env::var("SHOW").is_ok() {
                println!("--- page {page} ---\n{}", render(&mut app));
            }
            app.key(Key::Select);
            app.key(Key::Down);
        }
        app.key(Key::Right);
    }
    assert_eq!(app.page, 0);
}

#[test]
fn flips_after_hold_and_rotates_keys() {
    let mut app = App::new();
    let mut s = sensors();
    s.accel = [0.0, -0.9, 0.3];
    app.update(100, s);
    assert!(!app.flipped(), "needs to hold before flipping");
    for _ in 0..4 {
        app.update(100, s);
    }
    assert!(app.flipped());
    app.key(Key::Left); // physical left is screen right when flipped
    assert_eq!(app.page, 1);
    s.accel = [0.0, 0.9, 0.3];
    for _ in 0..5 {
        app.update(100, s);
    }
    assert!(!app.flipped());
}

#[test]
fn stress_many_frames() {
    let mut app = App::new();
    let keys = [Key::Right, Key::Select, Key::Down, Key::Up, Key::Left];
    for i in 0..2000u32 {
        let mut s = sensors();
        s.touch = (i % 7) as u8;
        app.update(33, s);
        if i % 5 == 0 {
            app.key(keys[(i as usize / 5) % keys.len()]);
        }
        render(&mut app);
    }
}

#[test]
fn demo_starts_on_held_select_cycles_pages_and_first_press_only_wakes() {
    let mut app = App::new();
    for _ in 0..(60_000 / 100) {
        app.update(100, sensors());
    }
    assert!(!app.demo(), "demo must not start by itself");
    let mut held = sensors();
    held.buttons = 0b10000;
    for _ in 0..11 {
        app.update(100, held);
    }
    assert!(app.demo());
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..(80_000 / 50) {
        app.update(50, sensors());
        seen.insert(app.page);
        render(&mut app);
    }
    assert!(seen.len() >= 5, "demo visited {seen:?}");
    app.key(Key::Right);
    assert!(!app.demo());
    assert_eq!(app.page, 0, "waking press should not navigate");
    app.key(Key::Right);
    assert_eq!(app.page, 1);
}

#[test]
fn touch_wakes_demo() {
    let mut app = App::new();
    app.start_demo();
    app.update(100, sensors());
    assert!(app.demo());
    let mut s = sensors();
    s.touch = 0b1;
    app.update(33, s);
    assert!(!app.demo());
}
