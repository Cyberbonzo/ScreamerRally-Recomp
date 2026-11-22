//! The visibility editor's window (`docs/05-visibility-editor.md`, "The editor window"): the
//! track from above with the game's textures, divided in sectors; the user turns sectors off
//! and on for the camera cell the game's camera is in (or one picked), and the game draws
//! exactly that (`sr_runtime::visedit` holds the shared map, camera, car and edits).
//!
//! It runs on its own thread with its own top-level window and Direct3D 11 device, never
//! touching the game's: [`spawn`] starts it, and closing it saves the edits and leaves the
//! game running.
//!
//! Modules: `window` (Win32 window, input as events), `editor` (state and behaviour),
//! `view` (zoom and pan, pixels and world), `scope` (camera cells an edit applies to,
//! rectangles), `mesh` (the map's triangles and textures on the CPU), `gpu` (Direct3D),
//! `panel` / `canvas` (the panel and status line in the overlay's pixel font), `shapes`
//! (outlines, car arrow), `script` (the test script).
//!
//! Diagnostics: `SR_VISEDIT_SHOT=DIR[,EVERY]` saves every EVERY-th frame drawn (0: only the
//! script's `shot` steps) as PNG in DIR; `SR_VISEDIT_SCRIPT` runs a test script
//! (`script.rs`).

mod action;
mod canvas;
mod editor;
mod gpu;
mod mesh;
mod panel;
mod scope;
mod script;
mod shapes;
mod view;
mod window;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor::{Btn, Editor, Event};
use script::Step;
use sr_runtime::visedit;

const TITLE: &str = "Screamer Rally - Visibility Editor";

/// Open the editor window on its own thread (it waits up to a few seconds for the game's
/// window, to open next to it).
pub fn spawn() -> Result<(), String> {
    std::thread::Builder::new()
        .name("visibility editor".into())
        .spawn(|| {
            if let Err(e) = run() {
                sr_runtime::error!("visibility editor: {e}");
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Where screenshots go (`SR_VISEDIT_SHOT=DIR[,EVERY]`).
struct Shots {
    dir: PathBuf,
    every: u64,
}

impl Shots {
    fn from_env() -> Shots {
        match std::env::var("SR_VISEDIT_SHOT") {
            Ok(v) => {
                let (dir, every) = v.split_once(',').map(|(d, n)| (d.to_string(), n.trim().parse().unwrap_or(0))).unwrap_or((v, 0));
                let _ = std::fs::create_dir_all(&dir);
                Shots { dir: dir.into(), every }
            }
            Err(_) => Shots { dir: PathBuf::from("."), every: 0 },
        }
    }

    fn save(&self, gpu: &gpu::Renderer, name: &str) {
        let path = self.dir.join(format!("{name}.png"));
        match gpu.capture() {
            Ok((w, h, px)) => match std::fs::write(&path, sr_render::png::encode(w, h, &px)) {
                Ok(()) => sr_runtime::info!("visibility editor: screenshot {}", path.display()),
                Err(e) => sr_runtime::warn!("visibility editor: cannot write {}: {e}", path.display()),
            },
            Err(e) => sr_runtime::warn!("visibility editor: screenshot failed: {e}"),
        }
    }
}

fn run() -> Result<(), String> {
    unsafe {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // next to the game's window: wait for it a little
    let t0 = Instant::now();
    let mut game = None;
    while game.is_none() && t0.elapsed() < Duration::from_secs(5) {
        game = window::game_window();
        if game.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let win = window::EditorWindow::create(TITLE, window::placement(game))?;
    let (w, h) = win.client_size();
    let mut ed = Editor::new(w, h, win.dpi());
    let mut gpu = gpu::Renderer::new(win.hwnd.0 as isize, (w, h))?;
    sr_runtime::info!("visibility editor: window open ({w} x {h}, {} dpi)", win.dpi());
    let shots = Shots::from_env();
    let mut script = match std::env::var("SR_VISEDIT_SCRIPT") {
        Ok(s) => {
            let text = if std::path::Path::new(&s).is_file() { std::fs::read_to_string(&s).map_err(|e| format!("{s}: {e}"))? } else { s };
            match script::parse(&text) {
                Ok(sc) => {
                    sr_runtime::info!("visibility editor: test script of {} steps from track {} (real mouse and keys ignored)", sc.steps.len(), sc.start_race);
                    Some(Runner { steps: sc.steps, start_race: sc.start_race, ..Default::default() })
                }
                Err(e) => {
                    sr_runtime::error!("visibility editor: SR_VISEDIT_SCRIPT: {e}");
                    None
                }
            }
        }
        Err(_) => None,
    };
    let mut title = String::new();
    let mut frames = 0u64;
    loop {
        if !win.pump() {
            break;
        }
        for ev in win.events() {
            let real_input = !matches!(ev, Event::Resize(..) | Event::Dpi(_));
            if !(real_input && script.is_some()) {
                ed.handle(ev);
            }
        }
        let paint = win.take_paint();
        if win.close_requested() {
            let close = match ed.save_before_closing() {
                Ok(()) => true,
                Err(e) => win.ask(&format!("The visibility edits could not be saved:\n{e}\n\nClose the editor anyway (the changes are lost)?")),
            };
            if close {
                visedit::set_highlight(None, false);
                win.destroy();
                continue;
            }
            win.cancel_close();
        }
        let now = Instant::now();
        ed.tick(now);
        if let Some(s) = &mut script {
            s.run(now, &mut ed, &win, &mut gpu, &shots);
        }
        if ed.needs_redraw() || paint {
            render(&mut ed, &mut gpu)?;
            frames += 1;
            if shots.every > 0 && frames % shots.every == 0 {
                shots.save(&gpu, &format!("visedit_{frames:05}"));
            }
            gpu.present();
            if !gpu.ok() {
                return Err("the Direct3D device was lost".into());
            }
        }
        let t = ed.title();
        if t != title {
            win.set_title(&t);
            title = t;
        }
        win.set_pointer(ed.pointer(), ed.mouse_inside());
        win.wait(if ed.busy() { 15 } else { 40 });
    }
    sr_runtime::info!("visibility editor: window closed");
    Ok(())
}

/// Draw a frame into the back buffer (not presented).
fn render(ed: &mut Editor, gpu: &mut gpu::Renderer) -> Result<(), String> {
    gpu.resize(ed.layout.size[0], ed.layout.size[1])?;
    if ed.map_changed {
        ed.map_changed = false;
        let map = ed.map().cloned();
        gpu.set_map(map.as_deref())?;
    }
    if ed.states_changed {
        ed.states_changed = false;
        gpu.upload_states(ed.states());
    }
    let panel = ed.paint_panel().clone();
    let status = ed.paint_status();
    let message = ed.map_message();
    let shapes = ed.shapes();
    gpu.draw(&gpu::Frame { layout: ed.layout, view: ed.view, tinted: ed.tinted(), hover: ed.hover(), textures: ed.textures(), shapes: &shapes.v, panel: &panel, status: &status, message: message.as_ref() });
    ed.drawn();
    Ok(())
}

/// The test script being run.
#[derive(Default)]
struct Runner {
    steps: Vec<(f64, Step)>,
    start_race: usize,
    next: usize,
    /// When the first track appeared (the steps' times count from there).
    start: Option<Instant>,
    mouse: [f64; 2],
    checks: usize,
    failed: usize,
    done: bool,
}

impl Runner {
    fn run(&mut self, now: Instant, ed: &mut Editor, win: &window::EditorWindow, gpu: &mut gpu::Renderer, shots: &Shots) {
        if self.done {
            return;
        }
        let start = match self.start {
            Some(s) => s,
            None if ed.races() >= self.start_race => *self.start.insert(now),
            None => return,
        };
        let t = now.duration_since(start).as_secs_f64();
        while self.next < self.steps.len() && self.steps[self.next].0 <= t {
            let step = self.steps[self.next].1.clone();
            self.next += 1;
            sr_runtime::info!("visibility editor script: {:.2} s: {step:?}", self.steps[self.next - 1].0);
            self.exec(step, ed, win, gpu, shots);
        }
        if self.next >= self.steps.len() {
            self.done = true;
            sr_runtime::info!("visibility editor script: finished, {} checks, {} failed", self.checks, self.failed);
        }
    }

    fn to(&mut self, ed: &mut Editor, p: [f64; 2]) {
        self.mouse = p;
        ed.handle(Event::Move(p[0], p[1]));
    }

    fn cell_px(ed: &Editor, c: (u16, u16)) -> [f64; 2] {
        let w = view::cell_center(c);
        ed.world_to_window(w[0], w[1])
    }

    fn exec(&mut self, step: Step, ed: &mut Editor, win: &window::EditorWindow, gpu: &mut gpu::Renderer, shots: &Shots) {
        let [x, y] = self.mouse;
        match step {
            Step::World(wx, wz) => {
                let p = ed.world_to_window(wx, wz);
                self.to(ed, p);
            }
            Step::Cell(r, c) => {
                let p = Self::cell_px(ed, (r, c));
                self.to(ed, p);
            }
            Step::Px(px, py) => self.to(ed, [px, py]),
            Step::Center(r, c, px) => ed.center_on((r, c), px),
            Step::Click { btn, ctrl } => {
                ed.handle(Event::Down { btn, x, y, ctrl });
                ed.handle(Event::Up { btn, x, y });
            }
            Step::Down(btn) => ed.handle(Event::Down { btn, x, y, ctrl: false }),
            Step::Up(btn) => ed.handle(Event::Up { btn, x, y }),
            Step::Drag { from, to, btn } => {
                let a = Self::cell_px(ed, from);
                let b = Self::cell_px(ed, to);
                self.to(ed, a);
                ed.handle(Event::Down { btn, x: a[0], y: a[1], ctrl: false });
                for k in 1..=8 {
                    let f = k as f64 / 8.0;
                    self.to(ed, [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
                }
                ed.handle(Event::Up { btn, x: b[0], y: b[1] });
            }
            Step::Wheel(n) => ed.handle(Event::Wheel { notches: n, x, y }),
            Step::Key { vk, ctrl, shift } => ed.handle(Event::Key { vk, ctrl, shift, repeat: false }),
            Step::Char(c) => ed.handle(Event::Char(c)),
            Step::Button(a) => {
                if ed.button_center(a).is_none() {
                    let _ = render(ed, gpu);
                }
                match ed.button_center(a) {
                    Some(p) => {
                        self.to(ed, p);
                        ed.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
                        ed.handle(Event::Up { btn: Btn::Left, x: p[0], y: p[1] });
                    }
                    None => {
                        self.checks += 1;
                        self.failed += 1;
                        sr_runtime::warn!("visibility editor script: no button {} on the panel: FAILED", a.id());
                    }
                }
            }
            Step::Shot(name) => {
                ed.tick(Instant::now());
                if render(ed, gpu).is_ok() {
                    shots.save(gpu, &name);
                    gpu.present();
                }
            }
            Step::Size(w, h) => win.set_client_size(w, h),
            Step::Check { cell, state } => {
                let s = ed.state(cell);
                let ok = match state.as_str() {
                    "drawn" => s.is_some_and(|s| s.drawn()),
                    "hidden" => s == Some(panel::SectorState::Hidden),
                    "added" => s == Some(panel::SectorState::Added),
                    "notdrawn" => s.is_some_and(|s| !s.drawn()),
                    _ => s == Some(panel::SectorState::DrawnByDefault),
                };
                self.check(ok, format!("sector {}, {} {state} from camera cell {:?} (is {s:?})", cell.0, cell.1, ed.cam()));
            }
            Step::CheckFile => {
                let r = ed.saved_file().ok_or_else(|| "nothing saved".to_string()).and_then(|p| {
                    let text = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
                    let (saved, id) = visedit::Edits::from_text(&text)?;
                    let same = saved == visedit::edits() && ed.map().is_some_and(|m| m.id == id);
                    Ok((p, same, saved.cams.len()))
                });
                match r {
                    Ok((p, same, n)) => self.check(same && !visedit::dirty(), format!("the file {p} holds the edits ({n} camera cells)")),
                    Err(e) => self.check(false, format!("the saved file: {e}")),
                }
            }
            Step::Log => ed.log_state(),
            Step::Close => win.post_close(),
        }
    }

    fn check(&mut self, ok: bool, what: String) {
        self.checks += 1;
        if ok {
            sr_runtime::info!("visibility editor script: check {what}: ok");
        } else {
            self.failed += 1;
            sr_runtime::warn!("visibility editor script: check {what}: FAILED");
        }
    }
}
