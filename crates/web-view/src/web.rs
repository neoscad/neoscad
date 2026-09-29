//! The wasm-bindgen class the page uses: `Viewer`.
//!
//! ```js
//! import init, { Viewer } from './web_view.js';
//! await init();
//! const viewer = await Viewer.create(canvas, { colorScheme: 'Cornfield' });
//! // From the worker: faces and edges as ArrayBuffers, meta as JSON.
//! viewer.setModel(new Uint8Array(faces), new Uint8Array(edges), meta, generation);
//! viewer.setSettings({ edges: true });
//! viewer.onClick((x, y) => pick(viewer.rayAt(x, y)));
//! ```
//!
//! # One thread, borrowed briefly
//!
//! Everything runs on the page's main thread, from event listeners and
//! animation frames. The viewport sits in a `RefCell` that each call
//! borrows only for as long as it needs, and page callbacks (`onClick`,
//! `onCameraChange`) are called with no borrow held, so a callback may
//! call straight back into the viewer. The listeners hold the shared state
//! weakly, so the viewer can be dropped (`free()` or `destroy()`) while
//! the canvas lives on.
//!
//! # Frames
//!
//! Every change marks the viewport dirty and asks for one animation frame;
//! a frame draws only if something changed (`Viewport::needs_draw`). An
//! idle view costs nothing, and a burst of pointer events in one frame
//! draws once.

// The viewport API shares its GPU and models through `Arc`s (the app's
// threads need them); wgpu's handles are not `Send` in a browser, where
// there is one thread, so here they are plain shared pointers.
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    AddEventListenerOptions, Event, EventTarget, HtmlCanvasElement, MediaQueryList, PointerEvent,
    ResizeObserver, ResizeObserverBoxOptions, ResizeObserverEntry, ResizeObserverOptions,
    ResizeObserverSize, WheelEvent,
};

use render::viewport::{Drawn, Gpu, Viewport};
use render::{ColorScheme, PackedScene};

use crate::input::{self, Action, Gestures};
use crate::wire;

/// What `Viewer.create` takes; every field is optional.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CreateOptions {
    /// A colour scheme name (`Viewer.colorSchemes()`); Cornfield if
    /// missing.
    color_scheme: Option<String>,
    /// `"auto"` (WebGPU, else WebGL2), `"webgpu"` or `"webgl"`.
    backend: Option<String>,
}

/// State the listeners and the animation frame share with the viewer.
struct Shared {
    canvas: HtmlCanvasElement,
    view: RefCell<Viewport>,
    gestures: RefCell<Gestures>,
    frame_pending: Cell<bool>,
    alive: Cell<bool>,
    /// Whether the browser reports the canvas's size in device pixels
    /// (`devicePixelContentBoxSize`; not Safari as of 2026), which is exact
    /// where CSS pixels times the ratio can be a pixel off.
    device_pixel_box: bool,
    on_click: RefCell<Option<js_sys::Function>>,
    on_camera: RefCell<Option<js_sys::Function>>,
    reported_camera: RefCell<Option<wire::CameraState>>,
}

type Listener = Closure<dyn FnMut(Event)>;

/// The 3D view on one canvas (see the module documentation).
#[wasm_bindgen]
pub struct Viewer {
    shared: Rc<Shared>,
    backend: String,
    adapter: String,
    listeners: Vec<(EventTarget, &'static str, Listener)>,
    observer: Option<ResizeObserver>,
    _observer_callback: Closure<dyn FnMut(js_sys::Array)>,
    /// The device-pixel-ratio watch, re-armed at each change.
    dpr_watch: Rc<RefCell<Option<(MediaQueryList, Listener)>>>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

fn window() -> web_sys::Window {
    web_sys::window().expect("the viewer runs in a window")
}

fn js_error(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn to_js<T: Serialize>(v: &T) -> JsValue {
    serde_json::to_string(v)
        .ok()
        .and_then(|s| js_sys::JSON::parse(&s).ok())
        .unwrap_or(JsValue::NULL)
}

/// A JavaScript value (an object, or a JSON string) as `T`; `undefined`
/// and `null` are `{}`, so every field takes its default.
fn from_js<T: DeserializeOwned>(v: &JsValue) -> Result<T, JsError> {
    let json = if v.is_undefined() || v.is_null() {
        "{}".to_string()
    } else if let Some(s) = v.as_string() {
        s
    } else {
        js_sys::JSON::stringify(v)
            .map_err(|_| JsError::new("the value cannot be written as JSON"))?
            .as_string()
            .unwrap_or_default()
    };
    serde_json::from_str(&json).map_err(js_error)
}

/// Log panics to the console instead of the bare `unreachable` trap.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&format!("neoscad web-view: {info}").into());
        }));
    });
}

/// Whether `ResizeObserverEntry` has `devicePixelContentBoxSize`.
/// Observing with that box where it is missing throws, so ask first.
fn supports_device_pixel_box() -> bool {
    let Ok(ctor) = js_sys::Reflect::get(&js_sys::global(), &"ResizeObserverEntry".into()) else {
        return false;
    };
    let Ok(proto) = js_sys::Reflect::get(&ctor, &"prototype".into()) else {
        return false;
    };
    proto.is_object()
        && js_sys::Reflect::has(&proto, &"devicePixelContentBoxSize".into()).unwrap_or(false)
}

impl Shared {
    /// Ask for an animation frame if something needs drawing and none is
    /// pending.
    fn schedule(self: &Rc<Self>) {
        if self.frame_pending.get() || !self.alive.get() || !self.view.borrow().needs_draw() {
            return;
        }
        self.frame_pending.set(true);
        let weak = Rc::downgrade(self);
        let callback = Closure::once_into_js(move |_time: f64| {
            if let Some(s) = weak.upgrade() {
                s.frame_pending.set(false);
                s.frame();
            }
        });
        if window()
            .request_animation_frame(callback.unchecked_ref())
            .is_err()
        {
            self.frame_pending.set(false);
        }
    }

    fn frame(self: &Rc<Self>) {
        if !self.alive.get() {
            return;
        }
        let drawn = self.view.borrow_mut().draw();
        match drawn {
            Ok(Drawn::Frame) => self.report_camera(),
            // The canvas could not take a frame now: the change is kept,
            // so the next frame tries again.
            Ok(Drawn::Deferred) => self.schedule(),
            Ok(Drawn::Idle) => {}
            Err(e) => web_sys::console::error_1(&format!("neoscad web-view: {e}").into()),
        }
    }

    /// Tell `onCameraChange` about a camera it has not seen.
    fn report_camera(&self) {
        let Some(f) = self.on_camera.borrow().clone() else {
            return;
        };
        let now = wire::CameraState::of(self.view.borrow().camera());
        if self.reported_camera.borrow().as_ref() == Some(&now) {
            return;
        }
        *self.reported_camera.borrow_mut() = Some(now);
        let _ = f.call1(&JsValue::NULL, &to_js(&now));
    }

    /// Size the viewport to the canvas: `device` is its size in device
    /// pixels when the browser reports it, else CSS pixels times the ratio.
    fn measure(self: &Rc<Self>, device: Option<(f64, f64)>) {
        let dpr = window().device_pixel_ratio();
        let dpr = if dpr.is_finite() && dpr > 0.0 {
            dpr
        } else {
            1.0
        };
        let (cw, ch) = (
            f64::from(self.canvas.client_width()),
            f64::from(self.canvas.client_height()),
        );
        let css = ((cw * dpr).round(), (ch * dpr).round());
        // The device-pixel box is exact where CSS size times the ratio can
        // round a pixel off, so it wins when the two nearly agree. When
        // they disagree by more, it is not describing the same pixels:
        // Chrome under device-scale emulation (Playwright's
        // `deviceScaleFactor`) reports the unscaled box with a ratio of 2,
        // and trusting it drew a half-resolution, blurry canvas.
        let (w, h) = match device {
            Some((dw, dh)) if (dw - css.0).abs() <= 2.0 && (dh - css.1).abs() <= 2.0 => (dw, dh),
            _ => css,
        };
        let (w, h) = (w.max(0.0) as u32, h.max(0.0) as u32);
        {
            let mut v = self.view.borrow_mut();
            if v.size() != (w, h) || v.scale() != dpr {
                v.resize(w, h, dpr);
            }
        }
        self.schedule();
    }

    fn apply(self: &Rc<Self>, action: Action) {
        {
            let mut v = self.view.borrow_mut();
            match action {
                Action::Orbit(dx, dy) => v.orbit(dx, dy),
                Action::Pan(dx, dy) => v.pan(dx, dy),
                Action::Zoom(notches) => v.with_camera(|c| c.zoom(120.0 * notches)),
                Action::Magnify(f) => v.with_camera(|c| c.zoom_by(f)),
                Action::Click(x, y) => {
                    drop(v);
                    let f = self.on_click.borrow().clone();
                    if let Some(f) = f {
                        let _ = f.call2(&JsValue::NULL, &x.into(), &y.into());
                    }
                    return;
                }
            }
        }
        self.schedule();
    }

    fn pointer_xy(&self, e: &PointerEvent) -> (f64, f64) {
        let r = self.canvas.get_bounding_client_rect();
        (
            f64::from(e.client_x()) - r.left(),
            f64::from(e.client_y()) - r.top(),
        )
    }

    fn on_event(self: &Rc<Self>, e: Event) {
        match e.type_().as_str() {
            "pointerdown" => {
                let Some(p) = e.dyn_ref::<PointerEvent>() else {
                    return;
                };
                let (x, y) = self.pointer_xy(p);
                let tracked =
                    self.gestures
                        .borrow_mut()
                        .press(p.pointer_id(), p.button(), p.alt_key(), x, y);
                if tracked {
                    let _ = self.canvas.set_pointer_capture(p.pointer_id());
                    // No text selection or native drag while orbiting.
                    e.prevent_default();
                }
            }
            "pointermove" => {
                let Some(p) = e.dyn_ref::<PointerEvent>() else {
                    return;
                };
                let (x, y) = self.pointer_xy(p);
                let actions = self.gestures.borrow_mut().motion(p.pointer_id(), x, y);
                for a in actions {
                    self.apply(a);
                }
            }
            "pointerup" | "pointercancel" => {
                let Some(p) = e.dyn_ref::<PointerEvent>() else {
                    return;
                };
                let cancel = e.type_() == "pointercancel";
                let action = self.gestures.borrow_mut().release(p.pointer_id(), cancel);
                if let Some(a) = action {
                    self.apply(a);
                }
            }
            "wheel" => {
                let Some(w) = e.dyn_ref::<WheelEvent>() else {
                    return;
                };
                // Keep the page from scrolling or zooming under the view.
                e.prevent_default();
                let action = input::wheel(
                    w.delta_x(),
                    w.delta_y(),
                    input::DeltaMode::from_dom(w.delta_mode()),
                    w.ctrl_key(),
                    w.shift_key(),
                );
                if let Some(a) = action {
                    self.apply(a);
                }
            }
            // A right drag pans; the menu would cover the view.
            "contextmenu" => e.prevent_default(),
            _ => {}
        }
    }
}

/// Watch for a change of the device pixel ratio (a window moved to
/// another display, or the page zoomed), which changes the canvas's
/// pixel size without always changing its CSS size, so a content-box
/// `ResizeObserver` alone would miss it. A media query matches only the
/// current ratio; it is replaced by one for the new ratio at each change.
fn watch_dpr(
    shared: Weak<Shared>,
    slot: Weak<RefCell<Option<(MediaQueryList, Listener)>>>,
) -> Option<(MediaQueryList, Listener)> {
    let dpr = window().device_pixel_ratio();
    let query = window()
        .match_media(&format!("(resolution: {dpr}dppx)"))
        .ok()??;
    let (s2, slot2) = (shared.clone(), slot.clone());
    let listener: Listener = Closure::new(move |_e: Event| {
        let Some(s) = s2.upgrade() else { return };
        s.measure(None);
        if let Some(slot) = slot2.upgrade() {
            // Replace this watch with one for the new ratio.
            let old = slot.borrow_mut().take();
            let new = watch_dpr(s2.clone(), slot2.clone());
            *slot.borrow_mut() = new;
            if let Some((q, l)) = old {
                let _ = q.remove_event_listener_with_callback("change", l.as_ref().unchecked_ref());
                // Dropping a closure while it runs would free the code
                // being executed: hand it to the JS garbage collector.
                l.forget();
            }
        }
    });
    query
        .add_event_listener_with_callback("change", listener.as_ref().unchecked_ref())
        .ok()?;
    Some((query, listener))
}

#[wasm_bindgen]
impl Viewer {
    /// Open the GPU on `canvas` and show an empty view. `options`:
    /// `{ colorScheme?: string, backend?: "auto" | "webgpu" | "webgl" }`.
    /// Fails (rejects) when neither WebGPU nor WebGL2 gives a device; the
    /// page then runs without a 3D view.
    pub async fn create(canvas: HtmlCanvasElement, options: JsValue) -> Result<Viewer, JsError> {
        install_panic_hook();
        let options: CreateOptions = from_js(&options)?;
        let scheme = match &options.color_scheme {
            Some(name) => render::scheme::find(name)
                .ok_or_else(|| JsError::new(&format!("no colour scheme called {name:?}")))?,
            None => ColorScheme::cornfield(),
        };
        let backends = match options.backend.as_deref() {
            Some("webgpu") => wgpu::Backends::BROWSER_WEBGPU,
            Some("webgl") => wgpu::Backends::GL,
            None | Some("auto") => wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            Some(other) => return Err(JsError::new(&format!("unknown backend {other:?}"))),
        };
        // WebGPU is chosen when the browser can make an adapter, not merely
        // when `navigator.gpu` exists (some browsers define it and then
        // give no adapter); otherwise WebGL2, when built with it.
        let instance = wgpu::util::new_instance_with_webgpu_detection(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
        .await;
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(js_error)?;
        let gpu = Arc::new(
            Gpu::with_surface(instance, &surface)
                .await
                .map_err(js_error)?,
        );
        let info = gpu.adapter_info();
        let backend = match info.backend {
            wgpu::Backend::BrowserWebGpu => "webgpu",
            wgpu::Backend::Gl => "webgl",
            _ => "other",
        }
        .to_string();
        let adapter = format!("{} ({:?})", info.name, info.backend);

        let dpr = window().device_pixel_ratio();
        let (w, h) = (
            (f64::from(canvas.client_width()) * dpr).round() as u32,
            (f64::from(canvas.client_height()) * dpr).round() as u32,
        );
        let mut view = Viewport::new(gpu, scheme).map_err(js_error)?;
        view.attach_surface(surface, w, h, dpr, false)
            .map_err(js_error)?;
        // Touch drags belong to the view, not to page scrolling.
        let _ = canvas.style().set_property("touch-action", "none");

        let shared = Rc::new(Shared {
            canvas: canvas.clone(),
            view: RefCell::new(view),
            gestures: RefCell::new(Gestures::default()),
            frame_pending: Cell::new(false),
            alive: Cell::new(true),
            device_pixel_box: supports_device_pixel_box(),
            on_click: RefCell::new(None),
            on_camera: RefCell::new(None),
            reported_camera: RefCell::new(None),
        });

        let mut listeners = Vec::new();
        let target: EventTarget = canvas.clone().into();
        for name in [
            "pointerdown",
            "pointermove",
            "pointerup",
            "pointercancel",
            "wheel",
            "contextmenu",
        ] {
            let weak = Rc::downgrade(&shared);
            let listener: Listener = Closure::new(move |e: Event| {
                if let Some(s) = weak.upgrade() {
                    s.on_event(e);
                }
            });
            let options = AddEventListenerOptions::new();
            // `wheel` must be able to prevent the page's scroll.
            options.set_passive(false);
            target
                .add_event_listener_with_callback_and_add_event_listener_options(
                    name,
                    listener.as_ref().unchecked_ref(),
                    &options,
                )
                .map_err(|_| JsError::new("cannot listen to the canvas"))?;
            listeners.push((target.clone(), name, listener));
        }
        // Page zoom changes the device pixel ratio and fires the window's
        // `resize`; the media-query watch below covers a move to another
        // display. Either way the canvas is measured again (a no-op when
        // nothing changed).
        {
            let weak = Rc::downgrade(&shared);
            let listener: Listener = Closure::new(move |_e: Event| {
                if let Some(s) = weak.upgrade() {
                    s.measure(None);
                }
            });
            let window: EventTarget = window().into();
            window
                .add_event_listener_with_callback("resize", listener.as_ref().unchecked_ref())
                .map_err(|_| JsError::new("cannot listen to the window"))?;
            listeners.push((window, "resize", listener));
        }

        let weak = Rc::downgrade(&shared);
        let observer_callback: Closure<dyn FnMut(js_sys::Array)> =
            Closure::new(move |entries: js_sys::Array| {
                let Some(s) = weak.upgrade() else { return };
                let device = if s.device_pixel_box {
                    entries
                        .get(0)
                        .dyn_into::<ResizeObserverEntry>()
                        .ok()
                        .and_then(|e| e.device_pixel_content_box_size().get(0).dyn_into().ok())
                        .map(|size: ResizeObserverSize| (size.inline_size(), size.block_size()))
                } else {
                    None
                };
                s.measure(device);
            });
        let observer = ResizeObserver::new(observer_callback.as_ref().unchecked_ref()).ok();
        if let Some(o) = &observer {
            if shared.device_pixel_box {
                let options = ResizeObserverOptions::new();
                options.set_box(ResizeObserverBoxOptions::DevicePixelContentBox);
                o.observe_with_options(&canvas, &options);
            } else {
                o.observe(&canvas);
            }
        }
        let dpr_watch = Rc::new(RefCell::new(None));
        *dpr_watch.borrow_mut() = watch_dpr(Rc::downgrade(&shared), Rc::downgrade(&dpr_watch));

        shared.measure(None);
        Ok(Viewer {
            shared,
            backend,
            adapter,
            listeners,
            observer,
            _observer_callback: observer_callback,
            dpr_watch,
        })
    }

    /// `"webgpu"` or `"webgl"`.
    #[wasm_bindgen(getter)]
    pub fn backend(&self) -> String {
        self.backend.clone()
    }

    /// The adapter's name and backend, for bug reports.
    #[wasm_bindgen(getter)]
    pub fn adapter(&self) -> String {
        self.adapter.clone()
    }

    /// Samples per pixel (4 with MSAA, 1 without).
    #[wasm_bindgen(getter)]
    pub fn samples(&self) -> u32 {
        self.shared.view.borrow().samples()
    }

    // --- The model ------------------------------------------------------

    /// Show a scene packed in the worker (`render::packed`): its face and
    /// edge bytes and its metadata (a JSON string or the parsed object).
    /// `generation` numbers the request; an older one arriving after a
    /// newer one is ignored (false). The first model is fitted (View All);
    /// later ones keep the camera. Throws on a scene that does not hold
    /// together, keeping the last model.
    #[wasm_bindgen(js_name = setModel)]
    pub fn set_model(
        &self,
        faces: Vec<u8>,
        edges: Vec<u8>,
        meta: JsValue,
        generation: f64,
    ) -> Result<bool, JsError> {
        let meta: render::PackedMeta = from_js(&meta)?;
        let packed = PackedScene { faces, edges, meta };
        let model = {
            let v = self.shared.view.borrow();
            v.gpu().upload_packed(&packed).map_err(js_error)?
        };
        drop(packed);
        let shown = self
            .shared
            .view
            .borrow_mut()
            .set_model(Arc::new(model), generation.max(0.0) as u64);
        self.shared.schedule();
        Ok(shown)
    }

    /// Show nothing (keeping the camera).
    #[wasm_bindgen(js_name = clearModel)]
    pub fn clear_model(&self) {
        self.shared.view.borrow_mut().clear_model();
        self.shared.schedule();
    }

    /// The model's bounding box, `[[x, y, z], [x, y, z]]`, or null.
    #[wasm_bindgen(js_name = modelBounds)]
    pub fn model_bounds(&self) -> JsValue {
        let v = self.shared.view.borrow();
        to_js(&v.model().and_then(|m| m.bounding_box()))
    }

    // --- The camera -----------------------------------------------------

    /// A drag by `dx`, `dy` points (y down): orbit.
    pub fn orbit(&self, dx: f64, dy: f64) {
        self.shared.apply(Action::Orbit(dx, dy));
    }

    /// Pan by `dx`, `dy` points, the model following the pointer.
    pub fn pan(&self, dx: f64, dy: f64) {
        self.shared.apply(Action::Pan(dx, dy));
    }

    /// Wheel zoom by `notches` (positive: closer), a tenth of the
    /// distance a notch as in OpenSCAD.
    pub fn zoom(&self, notches: f64) {
        self.shared.apply(Action::Zoom(notches));
    }

    /// Pinch zoom: the distance divided by `factor`.
    pub fn magnify(&self, factor: f64) {
        self.shared.apply(Action::Magnify(factor));
    }

    /// Turn about z by `degrees`.
    pub fn turn(&self, degrees: f64) {
        self.shared
            .view
            .borrow_mut()
            .with_camera(|c| c.turn(degrees));
        self.shared.schedule();
    }

    /// A standard view: `"top"`, `"bottom"`, `"left"`, `"right"`,
    /// `"front"`, `"back"` or `"diagonal"` (also `"iso"`), keeping the
    /// centre and distance as OpenSCAD's View menu does.
    #[wasm_bindgen(js_name = setView)]
    pub fn set_view(&self, name: &str) -> Result<(), JsError> {
        let name = if name == "diagonal" { "iso" } else { name };
        let view = render::snapshot::View::parse(name)
            .ok_or_else(|| JsError::new(&format!("no view called {name:?}")))?;
        self.shared.view.borrow_mut().set_view(view);
        self.shared.schedule();
        Ok(())
    }

    /// View All: centre on the model and fit it.
    #[wasm_bindgen(js_name = viewAll)]
    pub fn view_all(&self) {
        self.shared.view.borrow_mut().view_all();
        self.shared.schedule();
    }

    /// View > Center: look at the origin.
    pub fn center(&self) {
        self.shared
            .view
            .borrow_mut()
            .with_camera(render::Camera::center);
        self.shared.schedule();
    }

    /// Reset View: OpenSCAD's default camera.
    #[wasm_bindgen(js_name = resetView)]
    pub fn reset_view(&self) {
        self.shared.view.borrow_mut().reset_view();
        self.shared.schedule();
    }

    /// Look at a model point, keeping the rotation and distance.
    #[wasm_bindgen(js_name = lookAt)]
    pub fn look_at(&self, x: f64, y: f64, z: f64) {
        self.shared.view.borrow_mut().look_at([x, y, z]);
        self.shared.schedule();
    }

    /// The camera as `{vpt, vpr, vpd, vpf}`.
    pub fn camera(&self) -> JsValue {
        to_js(&wire::CameraState::of(self.shared.view.borrow().camera()))
    }

    /// The program's own view: whichever of `vpt`, `vpr`, `vpd` and `vpf`
    /// the object has, as OpenSCAD's GUI applies `$vp*` after a run. A view
    /// set so is not replaced by View All when the first model arrives.
    #[wasm_bindgen(js_name = setFileView)]
    pub fn set_file_view(&self, view: JsValue) -> Result<(), JsError> {
        let c: wire::CameraState = from_js(&view)?;
        self.shared
            .view
            .borrow_mut()
            .set_file_view(c.vpt, c.vpr, c.vpd, c.vpf);
        self.shared.schedule();
        Ok(())
    }

    /// Call `callback({vpt, vpr, vpd, vpf})` after a frame whose camera
    /// differs from the last one reported (null: stop).
    #[wasm_bindgen(js_name = onCameraChange)]
    pub fn on_camera_change(&self, callback: Option<js_sys::Function>) {
        *self.shared.on_camera.borrow_mut() = callback;
        *self.shared.reported_camera.borrow_mut() = None;
    }

    // --- Settings -------------------------------------------------------

    /// `{axes, scales, grid, edges, crosshairs, lighting, orthographic}`,
    /// `lighting` being `"openscad"` or `"headlight"`.
    pub fn settings(&self) -> JsValue {
        to_js(&self.current_settings())
    }

    /// Change the settings named in `settings` (the others stay).
    #[wasm_bindgen(js_name = setSettings)]
    pub fn set_settings(&self, settings: JsValue) -> Result<(), JsError> {
        let changes: serde_json::Value = from_js(&settings)?;
        let mut merged = serde_json::to_value(self.current_settings()).map_err(js_error)?;
        if let (Some(m), Some(c)) = (merged.as_object_mut(), changes.as_object()) {
            for (k, v) in c {
                m.insert(k.clone(), v.clone());
            }
        }
        let s: wire::Settings = serde_json::from_value(merged).map_err(js_error)?;
        {
            let mut v = self.shared.view.borrow_mut();
            v.set_settings(s.view());
            if v.camera().projection != s.projection() {
                v.set_projection(s.projection());
            }
        }
        self.shared.schedule();
        Ok(())
    }

    fn current_settings(&self) -> wire::Settings {
        let v = self.shared.view.borrow();
        wire::Settings::from_view(v.settings(), v.camera().projection)
    }

    /// The colour scheme's name.
    #[wasm_bindgen(js_name = colorScheme)]
    pub fn color_scheme(&self) -> String {
        self.shared.view.borrow().scheme().name.clone()
    }

    /// Draw the background and lines in the scheme called `name`. A
    /// model's face colours were fixed when the worker built its scene, so
    /// the page asks the worker to build it again in the new scheme.
    #[wasm_bindgen(js_name = setColorScheme)]
    pub fn set_color_scheme(&self, name: &str) -> Result<(), JsError> {
        let s = render::scheme::find(name)
            .ok_or_else(|| JsError::new(&format!("no colour scheme called {name:?}")))?;
        self.shared.view.borrow_mut().set_scheme(s);
        self.shared.schedule();
        Ok(())
    }

    /// Every colour scheme's name, in OpenSCAD's menu order.
    #[wasm_bindgen(js_name = colorSchemes)]
    pub fn color_schemes() -> Vec<String> {
        render::scheme::all().into_iter().map(|s| s.name).collect()
    }

    // --- Annotations and picking ----------------------------------------

    /// Draw `{lines: [{points: [[x,y,z]...], closed, color}], markers:
    /// [{point: [x,y,z], label, color}]}` over the model, replacing the
    /// ones before (colours `[r, g, b, a?]` from 0 to 1).
    #[wasm_bindgen(js_name = setAnnotations)]
    pub fn set_annotations(&self, annotations: JsValue) -> Result<(), JsError> {
        let a: wire::AnnotationSet = from_js(&annotations)?;
        self.shared
            .view
            .borrow_mut()
            .set_annotations(a.annotations());
        self.shared.schedule();
        Ok(())
    }

    /// The ray under a point of the canvas (CSS pixels from its top
    /// left), `{origin, direction}` in model coordinates, or null before
    /// the canvas has a size.
    #[wasm_bindgen(js_name = rayAt)]
    pub fn ray_at(&self, x: f64, y: f64) -> JsValue {
        let ray = self.shared.view.borrow().ray_at(x, y);
        match ray {
            Some((origin, direction)) => to_js(&wire::PickRay { origin, direction }),
            None => JsValue::NULL,
        }
    }

    /// Call `callback(x, y)` when the canvas is clicked (pressed and
    /// released without dragging), in CSS pixels from its top left
    /// (null: stop).
    #[wasm_bindgen(js_name = onClick)]
    pub fn on_click(&self, callback: Option<js_sys::Function>) {
        *self.shared.on_click.borrow_mut() = callback;
    }

    // --- Frames ---------------------------------------------------------

    /// Measure the canvas again (after a layout change the observer
    /// cannot see).
    pub fn resize(&self) {
        self.shared.measure(None);
    }

    /// Draw the next frame even if nothing changed.
    pub fn redraw(&self) {
        self.shared.view.borrow_mut().redraw();
        self.shared.schedule();
    }

    /// The current view drawn offscreen at `width` by `height` pixels,
    /// without the grid and annotations (File > Export's image): a promise
    /// of `{width, height, rgba}` (`rgba` a `Uint8Array`, top row first).
    pub fn image(&self, width: u32, height: u32) -> Result<js_sys::Promise, JsError> {
        let mut copy = self
            .shared
            .view
            .borrow()
            .copy_for_image(width, height)
            .map_err(js_error)?;
        let gpu = copy.gpu().clone();
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            // WebGL maps buffers only when the device is polled (WebGPU
            // does it from the browser's event loop, and polling it is a
            // no-op), so poll until the read is back.
            let poll = Closure::<dyn FnMut()>::new(move || {
                let _ = gpu.device().poll(wgpu::PollType::Poll);
            });
            let interval = window()
                .set_interval_with_callback_and_timeout_and_arguments_0(
                    poll.as_ref().unchecked_ref(),
                    4,
                )
                .ok();
            let image = copy.read_pixels().await;
            if let Some(id) = interval {
                window().clear_interval_with_handle(id);
            }
            drop(poll);
            let image = image.map_err(|e| JsValue::from(js_error(e)))?;
            let out = js_sys::Object::new();
            let rgba = js_sys::Uint8Array::from(image.rgba.as_slice());
            js_sys::Reflect::set(&out, &"width".into(), &image.width.into())?;
            js_sys::Reflect::set(&out, &"height".into(), &image.height.into())?;
            js_sys::Reflect::set(&out, &"rgba".into(), &rgba)?;
            Ok(out.into())
        }))
    }

    /// Stop listening to the canvas and release the GPU surface. The
    /// viewer does nothing afterwards; `free()` does this too.
    pub fn destroy(&mut self) {
        if !self.shared.alive.replace(false) {
            return;
        }
        for (target, name, listener) in self.listeners.drain(..) {
            let _ =
                target.remove_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        }
        if let Some(o) = self.observer.take() {
            o.disconnect();
        }
        if let Some((q, l)) = self.dpr_watch.borrow_mut().take() {
            let _ = q.remove_event_listener_with_callback("change", l.as_ref().unchecked_ref());
        }
        self.shared.view.borrow_mut().detach();
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.destroy();
    }
}
