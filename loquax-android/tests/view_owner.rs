//! The phone's map view moves only when the user moves it (1819, P0).
//!
//! Two background writers of the view lived in the facade
//! (`loquax_android::map`): a region that landed while the view sat
//! where the last fit left it fitted the view again (the newest region's,
//! with no pins: "zooming back into the desert", render walk finding 14),
//! and a frame asked without a view, which a `MapView` made anew asks
//! (an Activity recreated by a device rotation, finding 10), fitted the
//! view again instead of showing the one the user left.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use loquax::ui::map::MapPin;
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRequest, MapShown};

const W: u32 = 480;
const H: u32 = 640;

fn now() -> WallTime {
    WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    }
}

/// A frame as `MapView.java` asks it: its screen, the view it holds
/// (none on a `MapView` made anew).
fn request(home: &Path, view: Option<&MapShown>) -> MapRequest {
    MapRequest {
        dir: home.display().to_string(),
        w: W,
        h: H,
        density_milli: 1000,
        x: view.map(|v| v.x),
        y: view.map(|v| v.y),
        zoom_milli: view.map(|v| v.zoom_milli),
        about_x: (W / 2) as i32,
        about_y: (H / 2) as i32,
        ..MapRequest::default()
    }
}

fn frame(request: &MapRequest, pins: Vec<MapPin>) -> MapShown {
    let bytes = loquax_android::map::render(request, Some(pins), now());
    let (shown, _) = schema::unframe(&bytes).expect("a frame");
    assert!(shown.ok, "{:?}", shown.error);
    shown
}

fn view_of(shown: &MapShown) -> (u32, u32, u32) {
    (shown.x, shown.y, shown.zoom_milli)
}

/// A home with the Florence region and the autoload off.
fn florence_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home");
    let maps = home.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    std::fs::copy(fixture, maps.join("firenze.lmr")).expect("the region");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    home
}

/// Finding 10: the phone turned, its Activity made anew, the new
/// `MapView` asks its first frame without a view. The library shows the
/// view the user left, not the start's fit.
#[test]
fn a_map_view_made_anew_shows_the_view_the_user_left() {
    let home = florence_home();
    let first = frame(&request(home.path(), None), Vec::new());
    // The user drags the map: 120 px east, 80 px south.
    let dragged = frame(
        &MapRequest {
            pan_x: 120,
            pan_y: 80,
            moving: true,
            ..request(home.path(), Some(&first))
        },
        Vec::new(),
    );
    let rest = frame(&request(home.path(), Some(&dragged)), Vec::new());
    assert_ne!(
        view_of(&rest),
        view_of(&first),
        "the control: the drag moved it"
    );
    // Turned: a MapView made anew, its screen the other way round.
    let turned = frame(
        &MapRequest {
            w: H,
            h: W,
            ..request(home.path(), None)
        },
        Vec::new(),
    );
    assert_eq!(
        (turned.x, turned.y, turned.zoom_milli),
        view_of(&rest),
        "the rotation reset the view"
    );
    // An explicit "fit all" still fits: the user asked.
    let fit = frame(
        &MapRequest {
            fit: true,
            ..request(home.path(), Some(&turned))
        },
        Vec::new(),
    );
    assert_eq!(
        view_of(&fit),
        view_of(&first),
        "the control: fit is a command"
    );
}

/// Finding 14: a region that lands never moves the view. With no pins
/// the start's fit is the newest region's, Florence's; the screen
/// around it is not all covered, so the autoload asks for the screen's
/// box from the loopback server (the Oltrarno archive), which lands
/// while frames are asked at rest as Java asks them, the view fed back
/// each time. Every frame after the first shows the view of the first;
/// before 1819 the landed region, now the newest, was fitted to.
#[test]
fn a_region_landing_never_moves_the_phones_view() {
    use leviculum_mapcut::serve::Serve;
    const ARCHIVE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/oltrarno.pmtiles");
    let serve = Serve::start(vec![("o.pmtiles".into(), ARCHIVE.to_vec())]).expect("serve");
    let home = florence_home();
    let maps = home.path().join("maps");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        format!("map_source = \"{}\"\n", serve.url("o.pmtiles")),
    )
    .expect("config");
    let first = frame(&request(home.path(), None), Vec::new());
    let start = view_of(&first);
    let mut last = first;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut frames = 0usize;
    let mut landed_at: Option<usize> = None;
    loop {
        let shown = frame(&request(home.path(), Some(&last)), Vec::new());
        frames += 1;
        assert_eq!(
            view_of(&shown),
            start,
            "frame {frames}: the view moved with no input"
        );
        let regions = std::fs::read_dir(&maps)
            .expect("maps")
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "lmr"))
            .count();
        if regions > 1 && landed_at.is_none() {
            landed_at = Some(frames);
        }
        // Some frames after the landing: the refit came on the frame
        // that saw the loader's Done.
        if landed_at.is_some_and(|at| frames >= at + 5) && !shown.loading {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the control: no region landed ({frames} frames)"
        );
        last = shown;
        std::thread::sleep(Duration::from_millis(50));
    }
}
