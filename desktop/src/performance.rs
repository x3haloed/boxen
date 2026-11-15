//! Opt-in frame benchmark using the actual panel, observer and daemon replay.
use super::*;
use egui::{Color32, Event, RawInput, Rect, Shape, pos2, vec2};

fn has_visible_text(shape: &Shape, clip: Rect, needle: &str) -> bool {
    match shape {
        Shape::Text(text) => {
            text.galley.text().contains(needle) && text.visual_bounding_rect().intersects(clip)
        }
        Shape::Vec(shapes) => shapes.iter().any(|s| has_visible_text(s, clip, needle)),
        _ => false,
    }
}

fn percentile(values: &[f64], percentile: usize) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    values[(values.len() * percentile).div_ceil(100) - 1]
}

#[test]
#[ignore = "run with scripts/desktop-history-bench.py"]
fn retained_history_frames() {
    let env = |name| std::env::var(name).unwrap();
    let home = PathBuf::from(env("BOXEN_HISTORY_HOME"));
    let origin = env("BOXEN_HISTORY_ORIGIN");
    let count: usize = env("BOXEN_HISTORY_RUNS").parse().unwrap();
    let replies: usize = env("BOXEN_HISTORY_REPLIES").parse().unwrap();
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::dark());
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = vec2(8., 8.);
        style.visuals.selection.bg_fill = Color32::from_rgb(46, 72, 91);
    });
    let client = Client::new(origin.clone(), &home).unwrap();
    client.status().unwrap();
    let dest = client.local();
    let key = ThreadKey {
        peer: dest.key.clone(),
        id: env("BOXEN_HISTORY_THREAD"),
    };
    let mut app = Desktop {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        native_menu: None,
        data: Data::default(),
        client: Some(client.clone()),
        bus: Bus::new(ctx.clone()),
        state_path: home.join("desktop.json"),
        home,
        origin,
        polls: HashMap::new(),
        field: None,
        palette: false,
        palette_fresh: false,
        query: String::new(),
        choice: 0,
        connection: false,
        last_save: Instant::now(),
        next_watch: 0,
        next_discovery: Instant::now(),
        delivery_cursor: 0,
        ready: true,
        extra: None,
    };
    app.data.local_key = dest.key.clone();
    app.data.saved.destinations.push(dest.clone());
    let poll_stop = Arc::new(AtomicBool::new(false));
    client.catalog(dest, app.bus.publisher(), poll_stop.clone());
    let catalog_start = Instant::now();
    loop {
        app.updates();
        if find_thread(&app.data, &key).is_some_and(|thread| thread.runs.len() == count) {
            break;
        }
        assert!(catalog_start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(2));
    }
    let catalog_ms = catalog_start.elapsed().as_secs_f64() * 1000.;
    poll_stop.store(true, Ordering::Relaxed);
    app.action(Action::Open(key.clone(), false));
    let start = Instant::now();
    let newest_run = run_key(&key.peer, &format!("{count:064x}"));
    let mut first_latest_projected_ms = None;
    let mut first_latest_ms = None;
    let mut replay_ms = None;
    let mut replay_phases = Vec::new();
    let mut settled_phases = Vec::new();
    let mut draft_len = 0;
    while settled_phases.len() < 30 {
        let frame = Instant::now();
        app.updates();
        let applied = frame.elapsed();
        if first_latest_projected_ms.is_none()
            && app
                .data
                .transcripts
                .get(&newest_run)
                .is_some_and(|transcript| transcript.rows.iter().any(|row| row.id == "newest"))
        {
            first_latest_projected_ms = Some(start.elapsed().as_secs_f64() * 1000.);
        }
        let watch = Instant::now();
        app.observe();
        let selected = watch.elapsed();
        let complete = app.data.transcripts.len() == count
            && app
                .data
                .transcripts
                .values()
                .map(|t| t.seq as usize)
                .sum::<usize>()
                == count * (2 + 3 * replies) + 1;
        let type_now = !replay_phases.is_empty();
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(1000., 720.))),
            events: if type_now {
                vec![Event::Text("x".into())]
            } else {
                vec![]
            },
            ..Default::default()
        };
        let draw = Instant::now();
        let output = ctx.run_ui(input, |ui| {
            if !type_now {
                let target = attachments::Target::Thread(key.clone());
                let editor = ui.make_persistent_id(("composer", target));
                ui.memory_mut(|memory| memory.request_focus(editor));
            }
            let mut actions = Vec::new();
            thread_view::panel(ui, &mut app.data, &key, &mut actions);
            for action in actions {
                app.action(action);
            }
        });
        let laid_out = draw.elapsed();
        let latest_visible = output
            .shapes
            .iter()
            .any(|shape| has_visible_text(&shape.shape, shape.clip_rect, "Newest fixture message"));
        let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
        let frame_ms = frame.elapsed().as_secs_f64() * 1000.;
        if latest_visible {
            first_latest_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.);
        }
        if type_now {
            draft_len += 1;
            assert_eq!(
                app.data
                    .saved
                    .drafts
                    .iter()
                    .find(|(k, _)| k == &key)
                    .unwrap()
                    .1
                    .len(),
                draft_len,
                "An input event failed to reach the composer in its frame"
            );
        }
        let sample = [
            frame_ms,
            applied.as_secs_f64() * 1000.,
            selected.as_secs_f64() * 1000.,
            laid_out.as_secs_f64() * 1000.,
        ];
        if complete {
            replay_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.);
            settled_phases.push(sample);
        } else {
            replay_phases.push(sample);
        }
        assert!(start.elapsed() < Duration::from_secs(90));
        if !complete {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let stats = |samples: &[[f64; 4]]| {
        let [frame, apply, observe, layout] = [0, 1, 2, 3].map(|phase| {
            let values: Vec<_> = samples.iter().map(|s| s[phase]).collect();
            percentile(&values, 95)
        });
        json!({"frame_p95_ms":frame,"apply_p95_ms":apply,
               "observe_p95_ms":observe,"layout_p95_ms":layout,"frames":samples.len()})
    };
    let rows: usize = app
        .data
        .transcripts
        .values()
        .map(|transcript| {
            transcript
                .rows
                .iter()
                .filter(|row| conversation::is_message(row))
                .count()
        })
        .sum();
    assert_eq!(rows, count * (1 + replies) + 1);
    // The older run must be drawable even when its cached rectangle is outside
    // the clip. Resizing then recomputes the layout before jumping to the tail.
    for (activity, anchor, width, needle) in [
        (
            true,
            Some(format!("{:064x}:reply-0", 1)),
            1000.,
            "Turn 1, update 0.",
        ),
        (
            false,
            Some(format!("{:064x}:reply-0", 1)),
            1000.,
            "Turn 1, update 0.",
        ),
        (false, None, 780., "Newest fixture message"),
    ] {
        conversation::switch(&mut app.data, &key, activity, anchor);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut visible = false;
        while !visible && Instant::now() < deadline {
            let output = ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(width, 720.))),
                    ..Default::default()
                },
                |ui| thread_view::panel(ui, &mut app.data, &key, &mut Vec::new()),
            );
            visible = output
                .shapes
                .iter()
                .any(|shape| has_visible_text(&shape.shape, shape.clip_rect, needle));
        }
        assert!(visible, "Could not navigate to {needle} after replay");
    }
    println!(
        "HISTORY_BENCH {}",
        json!({
            "runs":count,"message_rows":rows,"catalog_ms":catalog_ms,
        "first_latest_projected_ms":first_latest_projected_ms.unwrap(),
            "first_latest_message_ms":first_latest_ms.expect("Latest response was never visible"),
            "full_replay_ms":replay_ms.unwrap(),"replay":stats(&replay_phases),
            "settled_typing":stats(&settled_phases),
        })
    );
}
