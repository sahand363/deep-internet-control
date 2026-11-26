//! Slint UI glue (v1.0). The UI never touches capture state: it
//! reads the `StateJson` snapshot the bridge publishes every 100 ms
//! and writes requests into `Shared`, exactly like the old egui UI.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::state::{AdapterSelection, AppView, BlockState, Shared, SortMode, StateJson};

slint::include_modules!();

const NORMAL_SIZE: (f32, f32) = (960.0, 660.0);
const MINIMAL_SIZE: (f32, f32) = (340.0, 150.0);

// ------------------------------------------------------------------ pure helpers

/// A round number at or above `v`, for the graph scale (1, 2, 5 x 10^n).
pub fn nice_ceiling(v: f64) -> f64 {
    if !v.is_finite() || v <= 1.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let base = 10f64.powf(exp);
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * base >= v {
            return m * base;
        }
    }
    10.0 * base
}

/// Average `values` into `points` buckets (keeps the newest samples).
#[allow(dead_code)] // kept for tests / future use
pub fn downsample(values: &[f64], points: usize) -> Vec<f64> {
    if values.is_empty() || points == 0 {
        return Vec::new();
    }
    if values.len() <= points {
        return values.to_vec();
    }
    let chunk = values.len() as f64 / points as f64;
    (0..points)
        .map(|i| {
            let a = (i as f64 * chunk) as usize;
            let b = (((i + 1) as f64 * chunk) as usize).min(values.len()).max(a + 1);
            values[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect()
}

/// SVG path commands for one series in a 1000x100 viewbox, plus the
/// closed area under it. The series is right-aligned (newest at the
/// right edge) over `slots` slots.
pub fn series_paths(values: &[f64], max: f64, slots: usize) -> (String, String) {
    let flat = "M 0 100 L 1000 100".to_string();
    if values.len() < 2 || slots < 2 {
        return (flat.clone(), "M 0 100 L 1000 100 L 1000 100 Z".to_string());
    }
    let step = 1000.0 / (slots - 1) as f64;
    let start = (slots - values.len()) as f64 * step;
    let pts: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let y = 100.0 - (v / max).clamp(0.0, 1.0) * 92.0;
            (start + i as f64 * step, y)
        })
        .collect();
    let mut line = format!("M {:.1} {:.1}", pts[0].0, pts[0].1);
    for (x, y) in &pts[1..] {
        line.push_str(&format!(" L {x:.1} {y:.1}"));
    }
    let area = format!("{line} L {:.1} 100 L {:.1} 100 Z", pts[pts.len() - 1].0, pts[0].0);
    (line, area)
}

/// First letter of an app name for its avatar.
pub fn initial_of(name: &str, unknown: bool) -> String {
    if unknown {
        return "?".to_string();
    }
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "•".to_string())
}

pub fn matches_search(name: &str, exe: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || name.to_lowercase().contains(&q) || exe.to_lowercase().contains(&q)
}

fn badge(state: BlockState) -> &'static str {
    match state {
        BlockState::Blocked => "BLOCKED",
        BlockState::BlockedNewConnectionsOnly => "BLOCKING",
        BlockState::NotBlocked => "",
    }
}

/// Replace a model's rows, touching only rows that changed (no
/// flicker, scroll position kept).
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&r) {
            model.set_row_data(i, r);
        }
    }
}

// ------------------------------------------------------------------ UI

struct UiState {
    hwnd: Option<isize>,
    applied_opacity: Option<f32>,
    identified: HashMap<String, String>,
    normal_size: (f32, f32),
    /// A native move/resize loop is running (v1.0.1): ignore further
    /// drag requests until the synthetic release has been delivered.
    dragging: bool,
    rates: SecondRates,
}

/// Graph length in one-second points.
pub const GRAPH_POINTS: usize = 60;

/// What the UI shows for speed (v1.0.1). Every number is the exact
/// byte count of the last full second (counter delta / elapsed time),
/// recomputed once per second — like Task Manager — so the digits are
/// accurate and calm, and the graph gets one fixed point per second
/// instead of re-bucketing 10 times a second.
#[derive(Default)]
pub struct SecondRates {
    last_at: Option<Instant>,
    last_session: (u64, u64),
    last_apps: HashMap<String, (u64, u64)>,
    pub top: (f64, f64),
    pub peak: (f64, f64),
    pub apps: HashMap<String, (f64, f64)>,
    pub adapters: HashMap<String, (String, String, bool)>,
    pub graph: VecDeque<(f64, f64)>,
}

/// Bytes/sec from two cumulative counters. A counter that went
/// backwards (selection changed, counters reset) reads as 0.
pub fn delta_rate(now: u64, before: u64, secs: f64) -> f64 {
    if now < before || secs <= 0.0 {
        0.0
    } else {
        (now - before) as f64 / secs
    }
}

impl SecondRates {
    /// Feed the latest snapshot. Returns true when a new one-second
    /// step was taken (the display should update).
    pub fn feed(&mut self, snap: &StateJson, now: Instant) -> bool {
        let Some(prev) = self.last_at else {
            self.last_at = Some(now);
            self.last_session = (snap.session.down, snap.session.up);
            self.last_apps = snap.apps.iter().map(|a| (a.exe_path.clone(), (a.down_total, a.up_total))).collect();
            self.graph = std::iter::repeat((0.0, 0.0)).take(GRAPH_POINTS).collect();
            self.capture_adapters(snap);
            return true;
        };
        let secs = now.duration_since(prev).as_secs_f64();
        if secs < 0.995 {
            return false;
        }
        self.top = (
            delta_rate(snap.session.down, self.last_session.0, secs),
            delta_rate(snap.session.up, self.last_session.1, secs),
        );
        self.last_session = (snap.session.down, snap.session.up);
        self.peak = (self.peak.0.max(self.top.0), self.peak.1.max(self.top.1));
        let mut apps = HashMap::new();
        let mut last = HashMap::new();
        for a in &snap.apps {
            let (d0, u0) = self.last_apps.get(&a.exe_path).copied().unwrap_or((a.down_total, a.up_total));
            apps.insert(
                a.exe_path.clone(),
                (delta_rate(a.down_total, d0, secs), delta_rate(a.up_total, u0, secs)),
            );
            last.insert(a.exe_path.clone(), (a.down_total, a.up_total));
        }
        self.apps = apps;
        self.last_apps = last;
        self.graph.push_back(self.top);
        while self.graph.len() > GRAPH_POINTS {
            self.graph.pop_front();
        }
        self.capture_adapters(snap);
        self.last_at = Some(now);
        true
    }

    fn capture_adapters(&mut self, snap: &StateJson) {
        self.adapters = snap
            .adapters
            .iter()
            .map(|a| (a.name.clone(), (a.down.clone(), a.up.clone(), a.down_bps > 0.0 || a.up_bps > 0.0)))
            .collect();
    }
}

/// Deliver a synthetic left-button release (and pointer exit) to Slint
/// after a native move/resize. Windows' modal move loop swallows the
/// real WM_LBUTTONUP, so without this Slint keeps the title-bar
/// TouchArea "pressed" and routes every later click to it — the v1.0.0
/// bug where all buttons stopped working a few seconds in.
fn release_pointer(ui: slint::Weak<AppWindow>, st: Rc<RefCell<UiState>>) {
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        if let Some(ui) = ui.upgrade() {
            use slint::platform::{PointerEventButton, WindowEvent};
            let w = ui.window();
            w.dispatch_event(WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(-1.0, -1.0),
                button: PointerEventButton::Left,
            });
            w.dispatch_event(WindowEvent::PointerExited);
        }
        st.borrow_mut().dragging = false;
    });
}

pub fn run(shared: Arc<Shared>) -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let (dark, opacity, minimal, startup, pinned, size) = {
        let s = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        (
            s.config.dark_theme,
            crate::opacity::clamp_opacity(s.config.opacity),
            s.config.minimal,
            s.config.run_on_startup,
            s.config.pinned,
            s.config.window_size.unwrap_or(NORMAL_SIZE),
        )
    };
    let normal_size = (size.0.max(560.0), size.1.max(420.0));
    ui.global::<Theme>().set_dark(dark);
    ui.invoke_apply_scheme();
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_win_opacity(opacity);
    ui.set_minimal(minimal);
    ui.set_run_on_startup(startup);
    ui.set_pinned(pinned);
    let start = if minimal { MINIMAL_SIZE } else { normal_size };
    ui.window().set_size(slint::LogicalSize::new(start.0, start.1));

    let apps_model: Rc<VecModel<AppRow>> = Rc::new(VecModel::default());
    let adapters_model: Rc<VecModel<AdapterRow>> = Rc::new(VecModel::default());
    let unknown_model: Rc<VecModel<UnknownRow>> = Rc::new(VecModel::default());
    ui.set_apps(ModelRc::from(apps_model.clone()));
    ui.set_adapters(ModelRc::from(adapters_model.clone()));
    ui.set_unknown_rows(ModelRc::from(unknown_model.clone()));

    let state = Rc::new(RefCell::new(UiState {
        hwnd: None,
        applied_opacity: None,
        identified: HashMap::new(),
        normal_size,
        dragging: false,
        rates: SecondRates::default(),
    }));
    let latest: Rc<RefCell<StateJson>> = Rc::new(RefCell::new(StateJson::default()));

    // ---- window controls
    {
        let st = state.clone();
        let w = ui.as_weak();
        ui.on_drag_window(move || {
            let h = { let s = st.borrow(); if s.dragging { None } else { s.hwnd } };
            if let Some(h) = h {
                st.borrow_mut().dragging = true;
                crate::win32::begin_drag(h, 0);
                release_pointer(w.clone(), st.clone());
            }
        });
    }
    {
        let st = state.clone();
        let w = ui.as_weak();
        ui.on_resize_window(move |edge| {
            let h = { let s = st.borrow(); if s.dragging { None } else { s.hwnd } };
            if let Some(h) = h {
                st.borrow_mut().dragging = true;
                crate::win32::begin_drag(h, edge);
                release_pointer(w.clone(), st.clone());
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_minimize_window(move || {
            if let Some(ui) = w.upgrade() {
                ui.window().set_minimized(true);
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_toggle_maximize(move || {
            if let Some(ui) = w.upgrade() {
                let m = !ui.window().is_maximized();
                ui.window().set_maximized(m);
                ui.set_win_max(m);
            }
        });
    }
    {
        let w = ui.as_weak();
        let sh = shared.clone();
        let st = state.clone();
        ui.on_close_window(move || {
            if let Some(ui) = w.upgrade() {
                save_size(&ui, &sh, &st);
                let _ = ui.hide();
            }
            let _ = slint::quit_event_loop();
        });
    }
    {
        let w = ui.as_weak();
        let sh = shared.clone();
        let st = state.clone();
        ui.on_minimal_changed(move |minimal| {
            let Some(ui) = w.upgrade() else { return };
            if minimal {
                save_size(&ui, &sh, &st);
                if ui.window().is_maximized() {
                    ui.window().set_maximized(false);
                    ui.set_win_max(false);
                }
                ui.window().set_size(slint::LogicalSize::new(MINIMAL_SIZE.0, MINIMAL_SIZE.1));
            } else {
                let s = st.borrow().normal_size;
                ui.window().set_size(slint::LogicalSize::new(s.0, s.1));
            }
            if let Ok(mut r) = sh.minimal_request.lock() {
                *r = Some(minimal);
            }
        });
    }
    {
        let sh = shared.clone();
        ui.on_theme_changed(move |dark| {
            let mut s = sh.store.lock().unwrap_or_else(|e| e.into_inner());
            s.config.dark_theme = dark;
            s.save();
        });
    }
    {
        let sh = shared.clone();
        ui.on_pin_changed(move |pinned| {
            let mut s = sh.store.lock().unwrap_or_else(|e| e.into_inner());
            s.config.pinned = pinned;
            s.save();
        });
    }
    {
        let sh = shared.clone();
        let st = state.clone();
        ui.on_opacity_changed(move |v| {
            let v = crate::opacity::clamp_opacity(v);
            if let Some(h) = st.borrow().hwnd {
                crate::opacity::apply_opacity(h, v);
            }
            st.borrow_mut().applied_opacity = Some(v);
            let mut s = sh.store.lock().unwrap_or_else(|e| e.into_inner());
            s.config.opacity = v;
            s.save();
        });
    }
    {
        let sh = shared.clone();
        ui.on_startup_changed(move |on| {
            if let Ok(mut r) = sh.startup_request.lock() {
                *r = Some(on);
            }
        });
    }

    // ---- apps
    {
        let sh = shared.clone();
        let l = latest.clone();
        ui.on_toggle_block(move |exe, block| {
            let can_block = l.borrow().can_block;
            set_block(&sh, exe.as_str(), block, can_block);
        });
    }
    ui.on_copy_text(|t| crate::win32::copy_to_clipboard(t.as_str()));
    ui.on_show_in_folder(|p| crate::win32::show_in_folder(p.as_str()));
    {
        let sh = shared.clone();
        ui.on_set_view_total(move |total| {
            if let Ok(mut r) = sh.view_request.lock() {
                *r = Some(if total { AppView::Total } else { AppView::Rate });
            }
        });
    }
    {
        let sh = shared.clone();
        ui.on_set_sort(move |m| {
            let mode = match m {
                1 => SortMode::Total,
                2 => SortMode::Name,
                _ => SortMode::Rate,
            };
            if let Ok(mut r) = sh.sort_request.lock() {
                *r = Some(mode);
            }
        });
    }
    {
        let st = state.clone();
        let l = latest.clone();
        ui.on_identify_unknown(move || {
            let samples = l.borrow().unknown_raw.clone();
            let tables = crate::connections::snapshot_tables();
            let mut st = st.borrow_mut();
            for s in &samples {
                let r = crate::unknown::identify_sample(s, &tables.tcp, &tables.udp, &|pid| {
                    crate::capture::exe_for_pid(pid)
                });
                st.identified.insert(s.key(), r.label());
            }
        });
    }

    // ---- adapters
    {
        let sh = shared.clone();
        ui.on_adapter_auto(move || set_adapter_mode(&sh, AdapterSelection::Auto));
    }
    {
        let sh = shared.clone();
        ui.on_adapter_all(move || set_adapter_mode(&sh, AdapterSelection::All));
    }
    {
        let sh = shared.clone();
        let l = latest.clone();
        ui.on_adapter_toggle(move |name, checked| {
            let current = l.borrow().adapter_mode.clone();
            let names: Vec<String> = match current {
                AdapterSelection::Named(n) => n,
                _ => Vec::new(),
            };
            let mode = match crate::state::toggle_named(&names, name.as_str(), checked) {
                Some(n) => AdapterSelection::Named(n),
                None => AdapterSelection::Auto,
            };
            set_adapter_mode(&sh, mode);
        });
    }

    // ---- 10 Hz refresh from the bridge snapshot
    let timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        let sh = shared.clone();
        let st = state.clone();
        let l = latest.clone();
        let apps_model = apps_model.clone();
        let adapters_model = adapters_model.clone();
        let unknown_model = unknown_model.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(ui) = w.upgrade() else { return };
            // Window handle: available once the window is shown.
            if st.borrow().hwnd.is_none() {
                if let Some(h) = crate::win32::hwnd_of(ui.window()) {
                    crate::win32::round_corners(h);
                    st.borrow_mut().hwnd = Some(h);
                }
            }
            {
                let mut s = st.borrow_mut();
                if let Some(h) = s.hwnd {
                    let want = crate::opacity::clamp_opacity(ui.get_win_opacity());
                    if s.applied_opacity != Some(want) {
                        crate::opacity::apply_opacity(h, want);
                        s.applied_opacity = Some(want);
                    }
                }
            }
            let snap = match sh.snapshot.read() {
                Ok(s) => s.clone(),
                Err(_) => return,
            };
            st.borrow_mut().rates.feed(&snap, Instant::now());
            refresh(&ui, &snap, &st.borrow(), &apps_model, &adapters_model, &unknown_model);
            *l.borrow_mut() = snap;
        });
    }

    let result = ui.run();
    save_size(&ui, &shared, &state);
    drop(timer);
    result
}

fn save_size(ui: &AppWindow, shared: &Arc<Shared>, st: &Rc<RefCell<UiState>>) {
    if ui.get_minimal() || ui.window().is_maximized() || ui.window().is_minimized() {
        return;
    }
    let size = ui.window().size().to_logical(ui.window().scale_factor());
    if size.width >= 300.0 && size.height >= 200.0 {
        let s = (size.width, size.height);
        st.borrow_mut().normal_size = s;
        let mut store = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        store.config.window_size = Some(s);
        store.save();
    }
}

fn set_adapter_mode(shared: &Arc<Shared>, mode: AdapterSelection) {
    if let Ok(mut r) = shared.adapter_mode_request.lock() {
        *r = Some(mode);
    }
}

/// Block / unblock one app: update the shared set (the blocker
/// engine follows it within ~250 ms), remember it in the config, and
/// on Block terminate the app's open TCP connections at once.
fn set_block(shared: &Arc<Shared>, exe_path: &str, block: bool, can_block: bool) {
    if block && !can_block {
        if let Ok(mut n) = shared.block_note.lock() {
            *n = Some(
                "Blocking is unavailable: the blocking engine could not start. \
                 Run Internet Control as administrator."
                    .to_string(),
            );
        }
        return;
    }
    let lower = exe_path.to_lowercase();
    if let Ok(mut set) = shared.blocked.write() {
        if block {
            set.insert(lower);
        } else {
            set.remove(&lower);
        }
    }
    {
        let mut store = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if block {
            if !store.config.blocked_exes.iter().any(|e| e.eq_ignore_ascii_case(exe_path)) {
                store.config.blocked_exes.push(exe_path.to_string());
            }
        } else {
            store.config.blocked_exes.retain(|e| !e.eq_ignore_ascii_case(exe_path));
        }
        store.save();
    }
    if block {
        let report = crate::connections::terminate_app_connections(exe_path);
        let note = report.summarize(exe_path);
        crate::log::log(&format!("Block {exe_path}: {note}"));
        if let Ok(mut n) = shared.block_note.lock() {
            *n = Some(note);
        }
    } else {
        crate::log::log(&format!("Unblock {exe_path}"));
        if let Ok(mut n) = shared.block_note.lock() {
            *n = None;
        }
    }
}

fn app_rate(st: &UiState, exe: &str) -> (String, String) {
    let (d, u) = st.rates.apps.get(exe).copied().unwrap_or((0.0, 0.0));
    (crate::meter::format_rate(d), crate::meter::format_rate(u))
}

fn refresh(
    ui: &AppWindow,
    snap: &StateJson,
    st: &UiState,
    apps_model: &VecModel<AppRow>,
    adapters_model: &VecModel<AdapterRow>,
    unknown_model: &VecModel<UnknownRow>,
) {
    let s = |v: &str| SharedString::from(v);
    let fr = crate::meter::format_rate;
    ui.set_down_rate(s(&fr(st.rates.top.0)));
    ui.set_up_rate(s(&fr(st.rates.top.1)));
    ui.set_session_down(s(&crate::meter::format_bytes(snap.session.down)));
    ui.set_session_up(s(&crate::meter::format_bytes(snap.session.up)));
    ui.set_today_down(s(&crate::meter::format_bytes(snap.today.down)));
    ui.set_today_up(s(&crate::meter::format_bytes(snap.today.up)));
    ui.set_peak_down(s(&fr(st.rates.peak.0)));
    ui.set_peak_up(s(&fr(st.rates.peak.1)));
    let ip = match &snap.public_ip {
        Some(p) => format!("Public IP  {p}"),
        None if !snap.selected_ip.is_empty() => format!("Public IP unavailable · local {}", snap.selected_ip),
        None => "Public IP unavailable".to_string(),
    };
    ui.set_ip_line(s(&ip));
    ui.set_adapter_line(s(if snap.selected_name.is_empty() { "—" } else { &snap.selected_name }));
    ui.set_status_line(s(&snap.capture_label));
    ui.set_capture_ok(snap.capture_active);
    ui.set_top_note(s(snap.top.note.as_deref().or(snap.selection_note.as_deref()).unwrap_or("")));
    ui.set_block_note(s(snap.block_note.as_deref().unwrap_or("")));
    ui.set_error_text(s(snap.error.as_deref().unwrap_or("")));
    ui.set_run_on_startup(snap.run_on_startup);
    ui.set_view_total(snap.app_view == AppView::Total);
    ui.set_sort_mode(match snap.sort_mode {
        SortMode::Rate => 0,
        SortMode::Total => 1,
        SortMode::Name => 2,
    });
    ui.set_adapter_mode(match snap.adapter_mode {
        AdapterSelection::Auto => 0,
        AdapterSelection::All => 1,
        AdapterSelection::Named(_) => 2,
    });

    // graph: one exact point per second, newest on the right
    let slots = GRAPH_POINTS;
    let d: Vec<f64> = st.rates.graph.iter().map(|p| p.0).collect();
    let u: Vec<f64> = st.rates.graph.iter().map(|p| p.1).collect();
    let peak = d.iter().chain(u.iter()).fold(0.0f64, |m, &v| m.max(v));
    let max = nice_ceiling(peak.max(1024.0));
    let (dl, da) = series_paths(&d, max, slots);
    let (ul, ua) = series_paths(&u, max, slots);
    ui.set_down_line(dl.into());
    ui.set_down_area(da.into());
    ui.set_up_line(ul.into());
    ui.set_up_area(ua.into());
    ui.set_graph_scale(s(&format!("max {}", crate::meter::format_rate(max))));

    // apps
    let query = ui.get_search().to_string();
    let total_view = snap.app_view == AppView::Total;
    let mut ordered: Vec<&crate::state::AppJson> = snap.apps.iter().collect();
    if snap.sort_mode == SortMode::Rate {
        // Order by the same one-second rates the rows show, so rows
        // only move once per second.
        let key = |a: &crate::state::AppJson| {
            let (d, u) = st.rates.apps.get(&a.exe_path).copied().unwrap_or((0.0, 0.0));
            d + u
        };
        ordered.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    }
    let rows: Vec<AppRow> = ordered
        .into_iter()
        .filter(|a| matches_search(&a.name, &a.exe_path, &query))
        .map(|a| {
            let unknown = a.exe_path == crate::capture::UNKNOWN_APP;
            let system = crate::unknown::is_system_exe(&a.exe_path);
            AppRow {
                exe: a.exe_path.clone().into(),
                name: a.name.clone().into(),
                down: if total_view { a.down_total_fmt.clone() } else { app_rate(st, &a.exe_path).0 }.into(),
                up: if total_view { a.up_total_fmt.clone() } else { app_rate(st, &a.exe_path).1 }.into(),
                blocked: a.blocked,
                badge: badge(a.block_state).into(),
                blockable: !unknown && !system,
                unknown,
                initial: initial_of(&a.name, unknown).into(),
            }
        })
        .collect();
    sync_model(apps_model, rows);

    // adapters
    let rows: Vec<AdapterRow> = snap
        .adapters
        .iter()
        .map(|a| AdapterRow {
            name: a.name.clone().into(),
            kind: if a.status.is_empty() { a.type_name.clone() } else { format!("{} · {}", a.type_name, a.status) }.into(),
            ip: a.ip.clone().unwrap_or_default().into(),
            down: st.rates.adapters.get(&a.name).map(|r| r.0.clone()).unwrap_or_default().into(),
            up: st.rates.adapters.get(&a.name).map(|r| r.1.clone()).unwrap_or_default().into(),
            total: a.total.clone().into(),
            checked: a.checked,
            selected: a.selected,
            unknown: a.unknown,
            active: st.rates.adapters.get(&a.name).map(|r| r.2).unwrap_or(false),
        })
        .collect();
    sync_model(adapters_model, rows);

    // unknown samples
    let rows: Vec<UnknownRow> = snap
        .unknown_samples
        .iter()
        .map(|u| UnknownRow {
            proto: u.protocol.clone().into(),
            local: u.local.clone().into(),
            remote: u.remote.clone().into(),
            bytes: u.bytes_fmt.clone().into(),
            ago: u.last_seen_ago.clone().into(),
            owner: st.identified.get(&u.key).cloned().unwrap_or_default().into(),
        })
        .collect();
    sync_model(unknown_model, rows);

    // diagnostics
    let dg = &snap.diagnostics;
    let mut lines = vec![
        format!("{} | packets {} | adapters {}", dg.capture_label, dg.packets, dg.adapters_enumerated),
        format!(
            "top numbers: {}{}",
            dg.top_source,
            if dg.fallback_active { " (all-interfaces fallback)" } else { "" }
        ),
    ];
    let per_if: Vec<String> = dg
        .capture_totals
        .iter()
        .map(|t| {
            format!(
                "#{} ↓{} ↑{}",
                t.index,
                crate::meter::format_bytes(t.down),
                crate::meter::format_bytes(t.up)
            )
        })
        .collect();
    if !per_if.is_empty() {
        lines.push(format!("by interface: {}", per_if.join("   ")));
    }
    ui.set_diagnostics(lines.join("\n").into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ceiling_rounds_up_to_1_2_5() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(3.0), 5.0);
        assert_eq!(nice_ceiling(1500.0), 2000.0);
        assert_eq!(nice_ceiling(10_000.0), 10_000.0);
        assert_eq!(nice_ceiling(60_000.0), 100_000.0);
    }

    #[test]
    fn downsample_averages_buckets() {
        let v: Vec<f64> = (0..10).map(|i| i as f64).collect();
        assert_eq!(downsample(&v, 5), vec![0.5, 2.5, 4.5, 6.5, 8.5]);
        assert_eq!(downsample(&v[..3], 5), vec![0.0, 1.0, 2.0]);
        assert!(downsample(&[], 5).is_empty());
    }

    #[test]
    fn series_paths_are_right_aligned_and_closed() {
        let (line, area) = series_paths(&[0.0, 50.0, 100.0], 100.0, 3);
        assert_eq!(line, "M 0.0 100.0 L 500.0 54.0 L 1000.0 8.0");
        assert!(area.ends_with("L 1000.0 100 L 0.0 100 Z"));
        let (line, _) = series_paths(&[1.0, 1.0], 1.0, 4);
        assert!(line.starts_with("M 666.7"));
        let (flat, _) = series_paths(&[], 1.0, 10);
        assert_eq!(flat, "M 0 100 L 1000 100");
    }

    #[test]
    fn delta_rate_is_exact_and_resets() {
        assert_eq!(delta_rate(3_000_000, 1_000_000, 2.0), 1_000_000.0);
        assert_eq!(delta_rate(5, 10, 1.0), 0.0);
        assert_eq!(delta_rate(10, 5, 0.0), 0.0);
    }

    #[test]
    fn initials_and_search() {
        assert_eq!(initial_of("chrome.exe", false), "C");
        assert_eq!(initial_of("_x", false), "X");
        assert_eq!(initial_of("anything", true), "?");
        assert!(matches_search("chrome.exe", r"C:\Program Files\chrome.exe", "CHROME"));
        assert!(matches_search("a", "b", "  "));
        assert!(!matches_search("a", "b", "z"));
    }
}
