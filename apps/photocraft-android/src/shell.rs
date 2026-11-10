//! The Android window: one eframe app around [`PhotocraftApp`], with the system file picker, the
//! S Pen and the app's private folders behind its [`Services`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use android_activity::AndroidApp;
use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_ui_egui::stylus::PenSample;
use photocraft_ui_egui::theme::ThemeKind;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogReply, FileDialogRequest, PhotocraftApp, Services};

use crate::jni_bridge::Java;

/// How often the window asks Java whether a picked file has arrived, while one is awaited.
const POLL: Duration = Duration::from_millis(150);
/// Frames between asks for the system bars' size.
const INSETS_EVERY: u64 = 30;
const PREFS_FILE: &str = "preferences.txt";

/// A picker request the system hasn't answered: whether it takes several files, and where the
/// answer goes.
struct Request {
    multiple: bool,
    reply: FileDialogReply,
}

/// The file picker's requests and answers, shared between the services and the window.
struct Picks {
    java: Arc<Java>,
    waiting: Mutex<VecDeque<Request>>,
    /// Files of the request being answered, collected until Java says it's done.
    arrived: Mutex<Vec<(String, Vec<u8>)>>,
    outgoing: PathBuf,
    counter: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Picks {
    fn open(&self, multiple: bool, reply: FileDialogReply) {
        lock(&self.waiting).push_back(Request { multiple, reply });
        if let Err(e) = self.java.call("startPick", &[if multiple { "place" } else { "open" }]) {
            log::error!("couldn't open the file picker: {e}");
            if let Some(request) = lock(&self.waiting).pop_back() {
                request.reply.send(Some(FileDialogAnswer::Unavailable(format!("the file picker couldn't be opened: {e}"))));
            }
        }
    }

    /// Hands a file to the system's save screen. The file is copied where the person chooses once
    /// they confirm; `Ok` means the screen was shown, not that they saved.
    fn save(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let clean: String = Path::new(name).file_name().map_or_else(|| "Untitled".into(), |n| n.to_string_lossy().into_owned());
        let folder = self.outgoing.join(self.counter.fetch_add(1, Ordering::SeqCst).to_string());
        std::fs::create_dir_all(&folder).map_err(|e| format!("couldn't prepare the file: {e}"))?;
        let path = folder.join(&clean);
        std::fs::write(&path, bytes).map_err(|e| format!("couldn't prepare the file: {e}"))?;
        self.java.call("startSave", &[&clean, &path.to_string_lossy()]).map(drop)
    }

    /// Answers the requests Java has finished. Returns whether any is still awaited.
    fn poll(&self) -> bool {
        if lock(&self.waiting).is_empty() {
            return false;
        }
        let events = match self.java.call("pollEvents", &[]) {
            Ok(events) => events,
            Err(e) => {
                log::error!("couldn't ask for picked files: {e}");
                return true;
            }
        };
        for line in events.lines() {
            let mut parts = line.splitn(3, '\t');
            let (kind, name, rest) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            match kind {
                "open" | "place" => match std::fs::read(rest) {
                    Ok(bytes) => {
                        lock(&self.arrived).push((name.to_string(), bytes));
                        let _ = std::fs::remove_file(rest);
                    }
                    Err(e) => log::error!("couldn't read the picked file {name}: {e}"),
                },
                "done" => self.finish_request(),
                "saved" => log::info!("saved {name}: {rest}"),
                "error" => log::error!("file picker: {name} {rest}"),
                _ => {}
            }
        }
        !lock(&self.waiting).is_empty()
    }

    fn finish_request(&self) {
        let Some(request) = lock(&self.waiting).pop_front() else { return };
        let mut files = std::mem::take(&mut *lock(&self.arrived));
        request.reply.send(match files.len() {
            0 => None,
            _ if request.multiple => Some(FileDialogAnswer::ContentsMany(files)),
            _ => {
                let (name, bytes) = files.swap_remove(0);
                Some(FileDialogAnswer::Contents(name, bytes))
            }
        });
    }
}

struct Shell {
    app: PhotocraftApp,
    picks: Arc<Picks>,
    java: Arc<Java>,
    /// The system bars' and cut-outs' size in points: top, bottom, left, right.
    insets: [f32; 4],
    /// The on-screen Shift key is latched on: the touch stand-in for holding Shift.
    shift_latched: bool,
}

fn services(picks: &Arc<Picks>, java: &Arc<Java>, root: &Path) -> Services {
    let (open, save, url_java) = (picks.clone(), picks.clone(), java.clone());
    let prefs = root.join(PREFS_FILE);
    let prefs_out = prefs.clone();
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            photocraft_io::export(doc, path, &settings.io_options()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        file_dialog: Some(Box::new(move |request, _parent, reply| match request {
            // The picker hands over the files' contents, not paths.
            FileDialogRequest::Open { multiple, .. } => open.open(multiple, reply),
            FileDialogRequest::Folder { .. } => reply.send(None),
            // The system's save screen comes with `write`: the suggested name is what it starts with.
            FileDialogRequest::Save { suggested } => {
                let name = Path::new(&suggested).file_name().map_or_else(|| suggested.clone(), |n| n.to_string_lossy().into_owned());
                reply.send(Some(FileDialogAnswer::SaveTo(name)));
            }
        })),
        write: Some(Box::new(move |path: &str, bytes: &[u8]| save.save(path, bytes))),
        // Choosing where to save happens after this call, and the person may cancel there.
        write_unconfirmed: true,
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        open_url: Some(Box::new(move |url: &str| url_java.call("openUrl", &[url]).map(drop))),
        load_prefs: Some(Box::new(move || std::fs::read_to_string(&prefs).ok())),
        save_prefs: Some(Box::new(move |text: &str| std::fs::write(&prefs_out, text).map_err(|e| e.to_string()))),
        preset_store: Some(photocraft_engine::preset_store::open_dir_async(root.join("presets"))),
        ..Default::default()
    }
}

impl Shell {
    fn new(cc: &eframe::CreationContext<'_>, java: Arc<Java>, root: &Path) -> Self {
        PhotocraftApp::install_type_reset_hooks(&cc.egui_ctx);
        PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Pro);
        let outgoing = root.join("outgoing");
        let _ = std::fs::remove_dir_all(&outgoing);
        let picks = Arc::new(Picks {
            java: java.clone(),
            waiting: Mutex::default(),
            arrived: Mutex::default(),
            outgoing,
            counter: AtomicUsize::new(0),
        });
        let mut app = PhotocraftApp::new(Session::new(), services(&picks, &java, root));
        app.set_theme(&cc.egui_ctx, ThemeKind::Pro);
        if let Some(render_state) = cc.wgpu_render_state.clone() {
            app.set_wgpu(render_state);
        }
        Self { app, picks, java, insets: [0.0; 4], shift_latched: false }
    }

    fn read_insets(&mut self, ctx: &egui::Context) {
        let Ok(text) = self.java.call("insets", &[]) else { return };
        let px: Vec<f32> = text.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if let [top, bottom, left, right] = px[..] {
            let scale = ctx.pixels_per_point().max(0.1);
            self.insets = [top / scale, bottom / scale, left / scale, right / scale];
        }
    }

    /// The on-screen Shift key: tap to hold Shift down, tap again to let go. It sits at the lower
    /// left, above the navigation bar, and lights up while the S Pen's button is held too.
    fn shift_key(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let pen = winit::platform::android::stylus_button_pressed();
        egui::Area::new(egui::Id::unique("android-shift-key"))
            .order(egui::Order::Foreground)
            .fixed_pos(area.left_bottom() + egui::vec2(12.0, -64.0))
            .show(ctx, |ui| {
                let label = egui::RichText::new("⇧ Shift").size(18.0);
                let button = egui::Button::new(label).selected(self.shift_latched || pen).min_size(egui::vec2(96.0, 44.0));
                if ui.add(button).on_hover_text("Hold Shift: keeps proportions, makes squares and circles. The S Pen button does the same.").clicked() {
                    self.shift_latched = !self.shift_latched;
                }
            });
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if ctx.cumulative_frame_nr() % INSETS_EVERY == 0 {
            self.read_insets(ctx);
        }
        if self.picks.poll() {
            ctx.request_repaint_after(POLL);
        }
        self.app.logic_frame(ctx, frame);
        self.app.finish_type_reset_logic(ctx);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Shift, without a keyboard: the S Pen's side button, or the on-screen Shift key. It keeps
        // proportions when scaling and makes squares and circles, like the key does.
        if self.shift_latched || winit::platform::android::stylus_button_pressed() {
            hold_shift(raw_input, ctx.input(|i| i.modifiers));
        }
        // The pen's pressure, tilt and eraser end, which touch events don't carry.
        self.app.stylus.feed.set(winit::platform::android::pen_reading().map(|p| PenSample {
            pressure: p.pressure,
            tilt_x: p.tilt_x,
            tilt_y: p.tilt_y,
            rotation: 0.0,
            eraser: p.eraser,
        }));
        eframe::App::raw_input_hook(&mut self.app, ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Keep the interface clear of the status bar, the navigation bar and a camera cut-out.
        let [top, bottom, left, right] = self.insets;
        let full = ui.max_rect();
        let area = egui::Rect::from_min_max(full.min + egui::vec2(left, top), full.max - egui::vec2(right, bottom));
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(area));
        self.app.ui_frame(&mut inner, frame);
        self.app.finish_type_reset_pass(ui.ctx());
        self.shift_key(ui.ctx(), area);
    }
}

/// Puts Shift on the frame and on every pointer, key and wheel event in it, as a keyboard's Shift
/// would be for a frame in which it is held. `current` keeps any other modifier a hardware
/// keyboard holds.
fn hold_shift(raw: &mut egui::RawInput, current: egui::Modifiers) {
    let held = egui::Modifiers { shift: true, ..current };
    raw.events.insert(0, egui::Event::ModifiersChanged(held));
    for event in &mut raw.events {
        match event {
            egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } | egui::Event::MouseWheel { modifiers, .. } => {
                modifiers.shift = true;
            }
            _ => {}
        }
    }
}

/// Runs the window until the activity finishes.
pub fn run(app: AndroidApp) -> Result<(), String> {
    let root = app.internal_data_path().ok_or("Android gave the app no private folder")?;
    let java = Arc::new(Java::new(&app)?);
    let mut options = eframe::NativeOptions { android_app: Some(app), renderer: eframe::Renderer::Wgpu, ..Default::default() };
    photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut options.wgpu_options.wgpu_setup);
    eframe::run_native("PhotoCraft", options, Box::new(move |cc| Ok(Box::new(Shell::new(cc, java, &root))))).map_err(|e| e.to_string())
}
