//! One world view as a PNG, for looking at the globe (1762):
//!
//!    cargo run --release -p leviculum-map --example globe_view -- \
//!        <lat> <lon> <zoom> <w> <h> <out.png>

use leviculum_map::blend::Blend;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::outline::{self, WORLD};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::Style;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 7 {
        eprintln!("usage: globe_view <lat> <lon> <zoom> <w> <h> <out.png>");
        std::process::exit(2);
    }
    let num = |i: usize| a[i].parse::<f64>().expect("number");
    let centre = project(LatLon::from_degrees(num(1), num(2)));
    let (w, h) = (num(4) as u32, num(5) as u32);
    let blend = Blend::new(centre, num(3), w, h);
    let mut fb = vec![0u16; (w * h) as usize];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(_) =
        outline::draw_world_view(&WORLD, &blend, Ink::Colour, Style::SCREEN.theme, &mut frame);
    let mut rgb = Vec::new();
    let frame = Frame::new(&mut fb, w, h).expect("frame");
    frame.rgb888_into(&mut rgb);
    let mut bytes = Vec::new();
    {
        let mut e = png::Encoder::new(&mut bytes, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().expect("png header");
        wr.write_image_data(&rgb).expect("png data");
    }
    std::fs::write(&a[6], bytes).expect("write png");
    println!("t={:.3}", blend.t());
}
