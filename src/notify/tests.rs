use super::*;
use anti_webui::core::{
    Ui, access, channel,
    event::{Modifiers, ScrollPhase, ScrollSource, WindowEvent},
    geometry::{PointF, Scale, SizeI},
    scene::{ImageDraw, Primitive, Scene},
};
use futures_lite::StreamExt;
use image::{Rgba, RgbaImage};
use std::io::Write;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use zbus::zvariant::Value;

#[test]
fn notification_file_urls_decode_to_real_local_images() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("space 中文 #100%.png");
    RgbaImage::from_pixel(2, 1, Rgba([20, 40, 60, 255]))
        .save(&path)
        .unwrap();
    let url = url::Url::from_file_path(&path).unwrap();

    let decoded = named_image_path(url.as_str(), 64).expect("local file URL");
    assert_eq!(decoded, path);
    let bitmap = anti_webui::core::images::decode_bitmap(decoded, (2, 1)).unwrap();
    assert_eq!((bitmap.width(), bitmap.height()), (2, 1));
    assert_eq!(
        named_image_path(path.to_str().unwrap(), 64).as_ref(),
        Some(&path)
    );

    for source in [
        "",
        "file://example.com/tmp/picture.png",
        "https://example.com/picture.png",
        "file://%",
    ] {
        assert_eq!(named_image_path(source, 64), None, "{source}");
    }
}

const BUS_NAME: &str = "org.freedesktop.Notifications";
const BUS_PATH: &str = "/org/freedesktop/Notifications";

fn send_notification(
    client: &zbus::Connection,
    replaces: u32,
    app_icon: &str,
    summary: &str,
    actions: &[&str],
    hints: HashMap<&str, Value<'_>>,
) -> u32 {
    zbus::block_on(async {
        client
            .call_method(
                Some(BUS_NAME),
                BUS_PATH,
                Some(BUS_NAME),
                "Notify",
                &(
                    "Image test",
                    replaces,
                    app_icon,
                    summary,
                    "notification body",
                    actions,
                    hints,
                    0i32,
                ),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    })
}

fn image_data(pixel: [u8; 4]) -> Value<'static> {
    Value::from((
        2i32,
        1i32,
        8i32,
        true,
        8i32,
        4i32,
        vec![
            pixel[0], pixel[1], pixel[2], pixel[3], pixel[0], pixel[1], pixel[2], pixel[3],
        ],
    ))
}

fn resident(hints: &mut HashMap<&str, Value<'_>>) {
    hints.insert("resident", Value::from(true));
}

fn scene_images(scene: &Scene) -> Vec<ImageDraw> {
    fn collect(scene: &Scene, out: &mut Vec<ImageDraw>) {
        for primitive in &scene.primitives {
            match primitive {
                Primitive::Image(image) => out.push(image.clone()),
                Primitive::Layer(layer) => collect(&layer.content, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(scene, &mut out);
    out
}

fn label_count(tree: &access::AccessTree, label: &str) -> usize {
    tree.nodes
        .iter()
        .filter(|node| node.label.as_deref() == Some(label))
        .count()
}

fn button_count(tree: &access::AccessTree, label: &str) -> usize {
    tree.nodes
        .iter()
        .filter(|node| node.role == access::Role::Button && node.label.as_deref() == Some(label))
        .count()
}

fn paint_now(ui: &mut Ui) -> Scene {
    let now = Instant::now() + Duration::from_secs(2);
    ui.update();
    ui.tick(now);
    ui.animate(now);
    ui.update();
    ui.paint(now)
}

fn render(ui: &mut Ui) -> Scene {
    ui.update();
    ui.wait_for_images(Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = Instant::now();
        ui.update();
        ui.tick(now);
        ui.animate(now);
        ui.update();
        let _ = ui.paint(now);
        if !ui.animating() {
            ui.wait_for_images(Duration::from_secs(5));
            return paint_now(ui);
        }
        assert!(Instant::now() < deadline, "notification UI did not settle");
        thread::yield_now();
    }
}

fn first_pixel(bitmap: &Bitmap) -> Vec<u8> {
    let mut pixel = None;
    assert!(bitmap.with_levels(|level, _, pixels| {
        if level == 0 {
            pixel = Some(pixels[..4].to_vec());
        }
    }));
    pixel.unwrap()
}

#[test]
fn images_replace_in_popups_and_history() {
    if std::env::var("SHOJI_IMAGE_TEST_CHILD").as_deref() != Ok("1") {
        let status = Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "notify::tests::images_replace_in_popups_and_history",
                "--nocapture",
            ])
            .env("SHOJI_IMAGE_TEST_CHILD", "1")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "private notification scenario failed: {status}"
        );
        return;
    }

    crate::settings::never_save();
    crate::settings::settings().set(Default::default());
    let directory = tempfile::tempdir().unwrap();
    let icon_path = directory.path().join("application icon.png");
    RgbaImage::from_pixel(2, 1, Rgba([90, 80, 70, 255]))
        .save(&icon_path)
        .unwrap();
    let picture_path = directory.path().join("picture on disk.png");
    RgbaImage::from_pixel(4, 2, Rgba([10, 20, 30, 255]))
        .save(&picture_path)
        .unwrap();
    let damaged_path = directory.path().join("damaged.png");
    std::fs::write(&damaged_path, b"not an image").unwrap();
    let missing_path = directory.path().join("missing.png");

    let server = server().expect("notification daemon on the private bus");
    let client = zbus::block_on(zbus::Connection::session()).unwrap();
    let (actions_tx, actions_rx) = mpsc::channel();
    let (listener_ready, listener_started) = mpsc::channel();
    let listener = client.clone();
    thread::spawn(move || {
        zbus::block_on(async move {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(BUS_NAME)
                .unwrap()
                .member("ActionInvoked")
                .unwrap()
                .build();
            let mut stream = zbus::MessageStream::for_match_rule(rule, &listener, None)
                .await
                .unwrap();
            listener_ready.send(()).unwrap();
            while let Some(Ok(message)) = stream.next().await {
                let (id, key): (u32, String) = message.body().deserialize().unwrap();
                actions_tx.send((id, key)).unwrap();
            }
        });
    });
    listener_started
        .recv_timeout(Duration::from_secs(5))
        .unwrap();

    let region_slot = std::cell::Cell::new(None);
    let popup_server = server.clone();
    let mut ui = Ui::new(|| {
        let region = signal(InputRegion::Empty);
        region_slot.set(Some(region));
        div()
            .w(760.0)
            .h(800.0)
            .flex()
            .flex_row()
            .items_start()
            .child(popups(popup_server, region))
            .child(history(0).w(WIDTH))
    });
    let region = region_slot.get().unwrap();
    let (wake_tx, wake_rx) = mpsc::channel();
    ui.set_image_waker(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    ui.handle_event(WindowEvent::Resized {
        size: SizeI::new(760, 800),
        scale: Scale::from_f64(1.0),
    });
    ui.update();
    assert_eq!(region.get(), InputRegion::Empty);

    let mut hints = HashMap::new();
    hints.insert("image-data", image_data([255, 0, 0, 255]));
    hints.insert(
        "image-path",
        Value::from(picture_path.to_string_lossy().into_owned()),
    );
    resident(&mut hints);
    let id = send_notification(
        &client,
        0,
        icon_path.to_str().unwrap(),
        "Old summary",
        &["default", "Open", "old", "Old action"],
        hints,
    );
    let old_image = server.notifications()[0].image.as_ref().unwrap().id();
    let scene = render(&mut ui);
    let tree = access::snapshot();
    assert_eq!(label_count(&tree, "Old summary"), 2);
    let summary_bounds = tree
        .nodes
        .iter()
        .filter(|node| node.label.as_deref() == Some("Old summary"))
        .map(|node| node.bounds)
        .collect::<Vec<_>>();
    assert!(
        summary_bounds.iter().all(|bounds| bounds.width > 0.0),
        "both summary nodes must be laid out: {summary_bounds:?}"
    );
    assert_eq!(button_count(&tree, "Old action"), 2);
    let images = scene_images(&scene);
    let pictures = images
        .iter()
        .filter(|image| image.bitmap.id() == old_image)
        .collect::<Vec<_>>();
    assert_eq!(
        pictures.len(),
        2,
        "memory image wins over image-path: {images:#?}"
    );
    assert!(
        pictures
            .iter()
            .all(|image| image.rect.height == 144 && image.rect.width == 288),
        "2:1 picture is contained, not cropped: {pictures:?}"
    );
    let icons = images
        .iter()
        .filter(|image| image.bitmap.id() != old_image && image.rect.height <= 36)
        .collect::<Vec<_>>();
    assert_eq!(icons.len(), 2, "the app icon keeps its own small slot");
    assert!(icons.iter().all(|image| image.rect.width <= 36));
    assert!(
        pictures.iter().map(|i| i.rect.y).min().unwrap()
            > icons.iter().map(|i| i.rect.y).max().unwrap()
    );

    let mut hints = HashMap::new();
    hints.insert("image-data", image_data([0, 255, 0, 255]));
    hints.insert(
        "image-path",
        Value::from(picture_path.to_string_lossy().into_owned()),
    );
    resident(&mut hints);
    assert_eq!(
        send_notification(
            &client,
            id,
            icon_path.to_str().unwrap(),
            "New summary",
            &["new", "New action"],
            hints,
        ),
        id
    );
    let new_image = server.notifications()[0].image.as_ref().unwrap().id();
    let scene = render(&mut ui);
    let tree = access::snapshot();
    assert_eq!(label_count(&tree, "New summary"), 2);
    assert_eq!(label_count(&tree, "Old summary"), 0);
    assert_eq!(button_count(&tree, "New action"), 2);
    assert_eq!(button_count(&tree, "Old action"), 0);
    let images = scene_images(&scene);
    assert!(images.iter().all(|image| image.bitmap.id() != old_image));
    assert_eq!(
        images
            .iter()
            .filter(|image| image.bitmap.id() == new_image)
            .count(),
        2
    );
    let action = tree.find(access::Role::Button, "New action").unwrap();
    assert!(access::perform(action.id, access::AccessAction::Click));
    ui.update();
    assert_eq!(
        actions_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (id, "new".to_owned())
    );
    assert!(actions_rx.try_recv().is_err());

    // Hold an old file decode open, replace its card with the same path plus memory pixels, then
    // release the old bytes. Its late callback must not restore the file image.
    let fifo_path = directory.path().join("late-picture");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .unwrap()
            .success()
    );
    let picture_bytes = std::fs::read(&picture_path).unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_path = fifo_path.clone();
    let writer = thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .unwrap();
        opened_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        file.write_all(&picture_bytes).unwrap();
    });
    let mut hints = HashMap::new();
    hints.insert(
        "image-path",
        Value::from(fifo_path.to_string_lossy().into_owned()),
    );
    resident(&mut hints);
    send_notification(
        &client,
        id,
        icon_path.to_str().unwrap(),
        "Waiting for path",
        &["new", "New action"],
        hints,
    );
    let _ = paint_now(&mut ui);
    opened_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let mut hints = HashMap::new();
    hints.insert("image-data", image_data([0, 0, 255, 255]));
    hints.insert(
        "image-path",
        Value::from(fifo_path.to_string_lossy().into_owned()),
    );
    resident(&mut hints);
    send_notification(
        &client,
        id,
        icon_path.to_str().unwrap(),
        "Latest memory image",
        &["new", "New action"],
        hints,
    );
    let latest_image = server.notifications()[0].image.as_ref().unwrap().id();
    let scene = paint_now(&mut ui);
    assert_eq!(
        scene_images(&scene)
            .iter()
            .filter(|image| image.bitmap.id() == latest_image)
            .count(),
        2
    );
    while wake_rx.try_recv().is_ok() {}
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    channel::pump();
    let scene = paint_now(&mut ui);
    assert_eq!(
        scene_images(&scene)
            .iter()
            .filter(|image| image.bitmap.id() == latest_image)
            .count(),
        2,
        "late completion from the disposed card must be ignored"
    );

    let mut hints = HashMap::new();
    hints.insert(
        "image-data",
        Value::from((0i32, 1i32, 0i32, true, 8i32, 4i32, Vec::<u8>::new())),
    );
    hints.insert(
        "image-path",
        Value::from(picture_path.to_string_lossy().into_owned()),
    );
    resident(&mut hints);
    send_notification(
        &client,
        id,
        icon_path.to_str().unwrap(),
        "Path fallback",
        &["new", "New action"],
        hints,
    );
    assert!(server.notifications()[0].image.is_none());
    let scene = render(&mut ui);
    let pictures = scene_images(&scene)
        .into_iter()
        .filter(|image| image.rect.height > 36)
        .collect::<Vec<_>>();
    assert_eq!(
        pictures.len(),
        2,
        "invalid image-data falls back to the file"
    );
    assert!(
        pictures
            .iter()
            .all(|image| first_pixel(&image.bitmap) == [10, 20, 30, 255])
    );

    let mut hints = HashMap::new();
    resident(&mut hints);
    send_notification(
        &client,
        id,
        icon_path.to_str().unwrap(),
        "No picture",
        &["new", "New action"],
        hints,
    );
    let scene = render(&mut ui);
    assert!(
        scene_images(&scene)
            .iter()
            .all(|image| image.rect.height <= 36)
    );
    assert_eq!(label_count(&access::snapshot(), "No picture"), 2);

    for (summary, path) in [
        ("Damaged picture", &damaged_path),
        ("Missing picture", &missing_path),
    ] {
        let mut hints = HashMap::new();
        hints.insert(
            "image-path",
            Value::from(path.to_string_lossy().into_owned()),
        );
        resident(&mut hints);
        send_notification(
            &client,
            id,
            icon_path.to_str().unwrap(),
            summary,
            &["new", "New action"],
            hints,
        );
        let scene = render(&mut ui);
        assert!(
            scene_images(&scene)
                .iter()
                .all(|image| image.rect.height <= 36)
        );
        assert_eq!(label_count(&access::snapshot(), summary), 2);
    }

    let mut hints = HashMap::new();
    hints.insert("image-data", image_data([200, 10, 10, 255]));
    resident(&mut hints);
    send_notification(
        &client,
        id,
        icon_path.to_str().unwrap(),
        "Bottom",
        &["bottom", "Bottom action"],
        hints,
    );
    for (summary, key, label, pixel) in [
        ("Middle", "middle", "Middle action", [10, 200, 10, 255]),
        ("Newest", "newest", "Newest action", [10, 10, 200, 255]),
    ] {
        let mut hints = HashMap::new();
        hints.insert("image-data", image_data(pixel));
        resident(&mut hints);
        send_notification(
            &client,
            0,
            icon_path.to_str().unwrap(),
            summary,
            &[key, label],
            hints,
        );
    }
    render(&mut ui);
    match region.get() {
        InputRegion::Rects(rects) => {
            assert!(!rects.is_empty());
            assert!(
                rects
                    .iter()
                    .all(|rect| rect.height <= POPUP_MAX_HEIGHT as i32)
            );
        }
        other => panic!("popups must have a bounded input region: {other:?}"),
    }
    ui.handle_event(WindowEvent::Scroll {
        position: PointF::new(100.0, 300.0),
        delta_x: 0.0,
        delta_y: 10000.0,
        modifiers: Modifiers::default(),
        source: ScrollSource::Unknown,
        phase: ScrollPhase::Single,
        time: 0,
    });
    paint_now(&mut ui);
    let tree = access::snapshot();
    let bottom = tree.find(access::Role::Button, "Bottom action").unwrap();
    assert!(
        bottom.bounds.y >= 0.0 && bottom.bounds.bottom() <= POPUP_MAX_HEIGHT,
        "scrolling must expose the last card's action: {:?}",
        bottom.bounds
    );
    assert!(access::perform(bottom.id, access::AccessAction::Click));
    ui.update();
    assert_eq!(
        actions_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (id, "bottom".to_owned())
    );

    server.clear();
    let start = Instant::now();
    for step in 0..12 {
        let now = start + Duration::from_millis(step * 100);
        ui.tick(now);
        ui.animate(now);
        ui.update();
    }
    assert_eq!(region.get(), InputRegion::Empty);
}
