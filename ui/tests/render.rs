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
    for page in 0..6 {
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
