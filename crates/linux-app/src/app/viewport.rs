//! The 3D view as a GTK widget: an overlay whose main child (a drawing
//! area with nothing to draw) sets the size and takes the mouse, and whose
//! overlaid picture shows the frames `linux_app::view::ViewCanvas` reads
//! back (why frames are copied rather than drawn into a surface:
//! `view.rs`). The picture is an overlay child so that its texture, which
//! is in device pixels, never becomes the widget's natural size.
//!
//! Frames are paced by the frame clock (a tick callback, GTK's display
//! link): each tick draws only if the viewport changed, so an idle view
//! costs a flag check a frame.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use linux_app::view::{Drag, ViewCanvas, apply_drag, drag_for, zoom};

/// The widget and the canvas it shows.
pub struct ViewWidget {
    pub root: gtk::Overlay,
    picture: gtk::Picture,
    pub canvas: Rc<RefCell<Option<ViewCanvas>>>,
}

impl std::fmt::Debug for ViewWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewWidget").finish_non_exhaustive()
    }
}

impl ViewWidget {
    /// A view drawing `canvas` (`None` without a GPU: the view then says
    /// so, and the rest of the window still works).
    pub fn new(canvas: Result<ViewCanvas, String>) -> ViewWidget {
        let area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        area.set_size_request(120, 120);
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Fill)
            .can_target(false)
            .build();
        let root = gtk::Overlay::builder().child(&area).build();
        root.add_overlay(&picture);
        root.add_css_class("view");
        let (canvas, error) = match canvas {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        };
        if let Some(e) = error {
            let page = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .title("No 3D view")
                .description(format!("NeoSCAD found no Vulkan or OpenGL device: {e}"))
                .build();
            page.add_css_class("compact");
            root.add_overlay(&page);
        }
        let canvas = Rc::new(RefCell::new(canvas));
        let w = ViewWidget {
            root,
            picture,
            canvas,
        };
        w.connect_input(&area);
        w.connect_frames(&area);
        w
    }

    /// Draw on the next frame (the model or the camera changed).
    pub fn queue(&self) {
        self.root.queue_draw();
    }

    fn connect_frames(&self, area: &gtk::DrawingArea) {
        let (canvas, picture) = (self.canvas.clone(), self.picture.clone());
        area.add_tick_callback(move |area, _clock| {
            let mut c = canvas.borrow_mut();
            let Some(c) = c.as_mut() else {
                return glib::ControlFlow::Continue;
            };
            let scale = area
                .native()
                .and_then(|n| n.surface())
                .map_or_else(|| f64::from(area.scale_factor()), |s| s.scale());
            c.set_size(area.width(), area.height(), scale);
            match c.frame() {
                Ok(Some(f)) => {
                    let texture = gdk::MemoryTexture::new(
                        f.width as i32,
                        f.height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &glib::Bytes::from_owned(f.rgba),
                        f.width as usize * 4,
                    );
                    picture.set_paintable(Some(&texture));
                }
                Ok(None) => {}
                Err(e) => glib::g_warning!("neoscad", "view frame failed: {e}"),
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_input(&self, area: &gtk::DrawingArea) {
        // Drags: left orbits, right (or Shift+left) pans, middle zooms.
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let state: Rc<RefCell<Option<(Drag, f64, f64)>>> = Rc::default();
        let s = state.clone();
        let a = area.clone();
        drag.connect_drag_begin(move |g, _, _| {
            a.grab_focus();
            let shift = g
                .current_event_state()
                .contains(gdk::ModifierType::SHIFT_MASK);
            *s.borrow_mut() = drag_for(g.current_button(), shift).map(|d| (d, 0.0, 0.0));
        });
        let (s, canvas) = (state.clone(), self.canvas.clone());
        drag.connect_drag_update(move |_, x, y| {
            let mut st = s.borrow_mut();
            let Some((d, lx, ly)) = st.as_mut() else {
                return;
            };
            let (dx, dy) = (x - *lx, y - *ly);
            (*lx, *ly) = (x, y);
            if let Some(c) = canvas.borrow_mut().as_mut() {
                apply_drag(&mut c.viewport, *d, dx, dy);
            }
        });
        let s = state;
        drag.connect_drag_end(move |_, _, _| *s.borrow_mut() = None);
        area.add_controller(drag);

        // The wheel and touchpad scrolling zoom (a wheel notch is 1).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let canvas = self.canvas.clone();
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(c) = canvas.borrow_mut().as_mut() {
                zoom(&mut c.viewport, -dy);
            }
            glib::Propagation::Stop
        });
        area.add_controller(scroll);

        // Pinch: the distance divided by the gesture's scale step.
        let pinch = gtk::GestureZoom::new();
        let last = Rc::new(RefCell::new(1.0f64));
        let l = last.clone();
        pinch.connect_begin(move |_, _| *l.borrow_mut() = 1.0);
        let canvas = self.canvas.clone();
        pinch.connect_scale_changed(move |_, scale| {
            let step = scale / *last.borrow();
            *last.borrow_mut() = scale;
            if let Some(c) = canvas.borrow_mut().as_mut()
                && step.is_finite()
                && step > 0.0
            {
                c.viewport.with_camera(|cam| cam.zoom_by(step));
            }
        });
        area.add_controller(pinch);
    }
}
