//! Test helper: drives a real (virtual) pointer through the compositor.
//! `cargo run --example vpointer -- W H x1 y1 x2 y2 ...` presses at the first
//! point, moves through the rest (holding the button) and releases at the last.
//! W/H are the output layout size the absolute coordinates are relative to.

use std::time::Duration;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_pointer, wl_registry::WlRegistry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

struct S;
impl Dispatch<WlRegistry, GlobalListContents> for S {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(impl Dispatch<$t, ()> for S {
        fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    })*};
}
ignore!(WlSeat, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1);

fn main() {
    let args: Vec<u32> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("numbers"))
        .collect();
    let (w, h) = (args[0], args[1]);
    let points: Vec<(u32, u32)> = args[2..].chunks(2).map(|c| (c[0], c[1])).collect();
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut queue) = registry_queue_init::<S>(&conn).unwrap();
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
    let mgr: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
    let vp = mgr.create_virtual_pointer(Some(&seat), &qh, ());
    let mut t = 0u32;
    let mut step = |f: &dyn Fn(u32), ms: u64| {
        t += ms as u32;
        f(t);
        vp.frame();
        queue.roundtrip(&mut S).unwrap();
        std::thread::sleep(Duration::from_millis(ms));
    };
    // VP_BUTTON=right sends right clicks.
    let button = if std::env::var("VP_BUTTON").as_deref() == Ok("right") {
        0x111
    } else {
        0x110
    };
    let (x0, y0) = points[0];
    step(&|t| vp.motion_absolute(t, x0, y0, w, h), 120);
    step(
        &|t| vp.button(t, button, wl_pointer::ButtonState::Pressed),
        120,
    );
    for win in points.windows(2) {
        let ((ax, ay), (bx, by)) = (win[0], win[1]);
        for i in 1..=20 {
            let f = i as f32 / 20.0;
            let (x, y) = (
                (ax as f32 + (bx as f32 - ax as f32) * f) as u32,
                (ay as f32 + (by as f32 - ay as f32) * f) as u32,
            );
            step(&|t| vp.motion_absolute(t, x, y, w, h), 25);
        }
        step(&|_| {}, 250);
    }
    step(
        &|t| vp.button(t, button, wl_pointer::ButtonState::Released),
        200,
    );
    vp.destroy();
    queue.roundtrip(&mut S).unwrap();
}
