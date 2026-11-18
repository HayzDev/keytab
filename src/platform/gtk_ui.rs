//! Native Linux renderer; the switcher owns all input and hint/search behavior.
use crate::switcher::{self, Key, Scope, View};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::keys::WinId;

struct Ui {
    window: gtk::Window,
    card: gtk::Frame,
    content: gtk::Box,
    previews: BTreeMap<WinId, gtk::gdk_pixbuf::Pixbuf>,
    capture: Option<Arc<AtomicBool>>,
    capture_running: bool,
    attempted: BTreeMap<WinId, Instant>,
    apps: Vec<gtk::gio::AppInfo>,
    right_shift: bool,
    search_header: gtk::Box,
    entry: gtk::Entry,
    updating_query: Rc<Cell<bool>>,
    searching: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

const CSS: &str = r#"
window#keytab { background-color: transparent; }
#keytab .keytab-card {
    background-color: rgba(19,21,27,0.84);
    background-image: linear-gradient(rgba(255,255,255,0.045), transparent);
    border: 1px solid rgba(255,255,255,0.22); border-radius: 8px;
    color: #ededf2; font: 14px sans-serif;
}
#keytab .keytab-card > border { border: none; }
#keytab .keytab-list, #keytab scrolledwindow, #keytab viewport { background: transparent; border: none; }
#keytab .keytab-row { padding: 6px 10px; border-radius: 6px; background: transparent; min-height: 28px; }
#keytab .keytab-row:selected { background: rgba(138,180,255,0.26); }
#keytab .keytab-row.closing { background: rgba(255,110,110,0.2); }
#keytab .keytab-app { font-weight: 600; }
#keytab .keytab-title, #keytab .keytab-help { color: #a8a8b3; }
#keytab .keytab-hint { border-radius: 5px; padding: 3px 4px; font: bold 13px monospace; }
#keytab .keytab-hint.window, #keytab .keytab-hint.app { color: #f2c35b; }
#keytab .keytab-hint.window { text-decoration-line: underline; }
#keytab .keytab-row:selected .keytab-hint { background-color: #8ab4ff; color: #0d1424; }
#keytab .keytab-row:selected .keytab-hint.window, #keytab .keytab-row:selected .keytab-hint.app { background-color: #f2c35b; color: #1d1500; }
#keytab .keytab-row.closing .keytab-hint { background-color: #ff8a8a; color: #0d1424; }
#keytab .keytab-query { font-size: 15px; padding: 10px 12px; margin: 8px 8px 0; border-radius: 6px; background: rgba(255,255,255,0.06); }
#keytab entry { background: transparent; border: none; box-shadow: none; color: #ededf2; padding: 0; min-height: 24px; }
#keytab .keytab-slot { border-radius: 6px; font: 24px monospace; }
#keytab .keytab-slot.on { border: 1px solid #f2c35b; color: #f2c35b; }
#keytab .keytab-error { color: #ff8a8a; font-size: 13px; }
#keytab .keytab-help { font-size: 12px; }
#keytab .keytab-footer { padding: 8px 16px 9px; border-top: 1px solid rgba(255,255,255,0.08); font-size: 12px; }
#keytab .keytab-keycap {
    color: #ededf2; font: 12px sans-serif; min-width: 14px; min-height: 18px;
    padding: 1px 5px; border-radius: 5px;
}
#keytab .keytab-keycap, #keytab .keytab-hint, #keytab .keytab-slot {
    background-color: rgba(255,255,255,0.07);
    background-image: linear-gradient(rgba(255,255,255,0.045), transparent);
    border: 1px solid rgba(255,255,255,0.1);
    box-shadow: inset 0 1px rgba(255,255,255,0.06);
}
#keytab .keytab-preview { background: transparent; border: 1px solid rgba(255,255,255,0.22); border-radius: 4px; padding: 0; }
#keytab .keytab-preview > border { border: none; }
#keytab .keytab-preview.unavailable { border: none; }
#keytab .keytab-monogram { background: #3b617d; border-radius: 5px; font-size: 12px; font-weight: bold; color: white; }
"#;

pub(crate) fn init(window: &gtk::Window) {
    window.set_widget_name("keytab");
    let css = gtk::CssProvider::new();
    css.load_from_data(CSS.as_bytes()).expect("native overlay CSS");
    gtk::StyleContext::add_provider_for_screen(
        &WidgetExt::screen(window).expect("GTK screen"), &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let root = gtk::Overlay::new();
    // A hidden card still needs a first transparent buffer to start the frame clock.
    let canvas = gtk::DrawingArea::new();
    canvas.set_size_request(640, 720);
    canvas.connect_draw(|_, cr| {
        cr.set_operator(gtk::cairo::Operator::Source);
        cr.set_source_rgba(0., 0., 0., 0.);
        let _ = cr.paint();
        cr.set_operator(gtk::cairo::Operator::Over);
        gtk::glib::Propagation::Proceed
    });
    canvas.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK);
    canvas.connect_button_press_event(|_, _| {
        switcher::hide();
        gtk::glib::Propagation::Stop
    });
    root.add(&canvas);
    let card = gtk::Frame::new(None);
    card.style_context().add_class("keytab-card");
    card.set_valign(gtk::Align::Start);
    card.set_margin_top(36);
    card.set_margin_start(24);
    card.set_margin_end(24);
    card.set_no_show_all(true);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add(&content);
    root.add_overlay(&card);
    if let Some(old) = window.child() { window.remove(&old); }
    window.add(&root);
    let native = window.clone();
    card.connect_size_allocate(move |card, rect| {
        if card.is_visible() {
            // GTK allocations can be relative to the widget's own GdkWindow.
            if let Some((x, y)) = card.translate_coordinates(&native, 0, 0) {
                super::niri::material(&native, x, y, rect.width(), rect.height());
            }
        }
    });
    let entry = gtk::Entry::new();
    entry.set_has_frame(false);
    entry.set_placeholder_text(Some("Search windows"));
    let search_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    search_header.style_context().add_class("keytab-query");
    search_header.pack_start(&gtk::Image::from_icon_name(Some("system-search-symbolic"), gtk::IconSize::Menu), false, false, 0);
    search_header.pack_start(&entry, true, true, 0);
    let updating_query = Rc::new(Cell::new(false));
    let updating = updating_query.clone();
    entry.connect_changed(move |entry| {
        if !updating.get() { switcher::set_query(entry.text().to_string()); }
    });
    window.connect_key_press_event(|_, event| {
        let input = key(event);
        let right_shift = UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            let Some(ui) = ui.as_mut() else { return false };
            if input == Key::Search { ui.right_shift = true; }
            ui.right_shift
        });
        if !matches!(input, Key::Preview | Key::Search) && !right_shift && event.state().contains(gtk::gdk::ModifierType::SHIFT_MASK) {
            switcher::on_key(Key::Preview, true);
        }
        let searching = UI.with(|ui| ui.borrow().as_ref().is_some_and(|ui| ui.searching));
        if searching && native_search_key(input, event.state()) {
            return gtk::glib::Propagation::Proceed;
        }
        switcher::on_key(input, true);
        gtk::glib::Propagation::Stop
    });
    window.connect_key_release_event(|_, event| {
        if key(event) == Key::Search {
            UI.with(|ui| { if let Some(ui) = ui.borrow_mut().as_mut() { ui.right_shift = false; } });
        }
        switcher::on_key(key(event), false);
        gtk::glib::Propagation::Stop
    });
    window.connect_focus_out_event(|_, _| {
        UI.with(|ui| { if let Some(ui) = ui.borrow_mut().as_mut() { ui.right_shift = false; } });
        gtk::glib::Propagation::Proceed
    });
    UI.with(|ui| *ui.borrow_mut() = Some(Ui { window: window.clone(), card, content, previews: BTreeMap::new(), capture: None, capture_running: false, attempted: BTreeMap::new(), apps: gtk::gio::AppInfo::all(), right_shift: false, search_header, entry, updating_query, searching: false }));
}

pub(crate) fn window() -> gtk::Window {
    UI.with(|ui| ui.borrow().as_ref().expect("native overlay").window.clone())
}

fn native_search_key(input: Key, modifiers: gtk::gdk::ModifierType) -> bool {
    use gtk::gdk::ModifierType;
    let ctrl = modifiers.contains(ModifierType::CONTROL_MASK);
    let alt = modifiers.contains(ModifierType::MOD1_MASK);
    match input {
        Key::Esc | Key::Enter | Key::Up | Key::Down | Key::Ctrl | Key::Alt | Key::Preview | Key::Search | Key::HintWindow | Key::HintApp => false,
        Key::Delete => ctrl || modifiers.contains(ModifierType::SHIFT_MASK),
        Key::Char('p') if ctrl => false,
        Key::Char('j' | 'k' | 'h') if alt => false,
        _ => true,
    }
}

fn key(event: &gtk::gdk::EventKey) -> Key {
    match event.keyval().name().as_deref().unwrap_or("") {
        "Escape" | "Caps_Lock" => Key::Esc,
        "BackSpace" => Key::Back,
        "Return" | "KP_Enter" => Key::Enter,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Delete" | "KP_Delete" => Key::Delete,
        "Alt_L" | "Alt_R" => Key::Alt,
        "Control_L" | "Control_R" => Key::Ctrl,
        "Shift_L" => Key::Preview,
        "Shift_R" => Key::Search,
        _ => event.keyval().to_unicode().map_or(Key::Other, |c| Key::from_dom(&c.to_string())),
    }
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.style_context().add_class(class);
    label.set_xalign(0.);
    label.set_single_line_mode(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn highlighted(text: &str, hits: &[usize], offset: usize) -> String {
    let mut out = String::new();
    let (mut start, mut marked) = (0, false);
    for (i, (byte, _)) in text.char_indices().enumerate() {
        let next = hits.contains(&(i + offset));
        if next != marked {
            out.push_str(&gtk::glib::markup_escape_text(&text[start..byte]));
            out.push_str(if next { "<span foreground='#8ab4ff' weight='bold' underline='single'>" } else { "</span>" });
            start = byte;
            marked = next;
        }
    }
    out.push_str(&gtk::glib::markup_escape_text(&text[start..]));
    if marked { out.push_str("</span>"); }
    out
}

fn hint_markup(hint: &str, typed: &str) -> String {
    let count = if hint.starts_with(typed) { typed.chars().count() } else { 0 };
    hint.chars().enumerate().map(|(i, c)| {
        let text = gtk::glib::markup_escape_text(&c.to_ascii_uppercase().to_string()).to_string();
        if i < count { format!("<span alpha='72%'>{text}</span>") } else { text }
    }).collect()
}

fn preview_pixels(width: i32, height: i32, display_scale: i32) -> (i32, i32) {
    let fit = (560. * display_scale as f64 / width as f64)
        .min(240. * display_scale as f64 / height as f64).min(1.);
    ((width as f64 * fit).max(1.) as i32, (height as f64 * fit).max(1.) as i32)
}

fn rounded_clip(cr: &gtk::cairo::Context, width: f64, height: f64, radius: f64) {
    let r = radius.min(width / 2.).min(height / 2.);
    let quarter = std::f64::consts::FRAC_PI_2;
    cr.new_sub_path();
    cr.arc(width - r, r, r, -quarter, 0.);
    cr.arc(width - r, height - r, r, 0., quarter);
    cr.arc(r, height - r, r, quarter, 2. * quarter);
    cr.arc(r, r, r, 2. * quarter, 3. * quarter);
    cr.close_path();
    cr.clip();
}

fn thumbnail(pixbuf: Option<&gtk::gdk_pixbuf::Pixbuf>, display_scale: i32) -> gtk::Frame {
    let frame = gtk::Frame::new(None);
    frame.style_context().add_class("keytab-preview");
    frame.set_margin_start(6);
    frame.set_margin_end(6);
    frame.set_margin_top(8);
    frame.set_margin_bottom(6);
    frame.set_halign(gtk::Align::Center);
    if let Some(pixbuf) = pixbuf {
        use gtk::gdk::prelude::GdkContextExt;
        let (width, height) = preview_pixels(pixbuf.width(), pixbuf.height(), display_scale);
        if let Some(scaled) = pixbuf.scale_simple(width, height, gtk::gdk_pixbuf::InterpType::Hyper) {
            if let Ok(surface) = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, width, height) {
                if let Ok(cr) = gtk::cairo::Context::new(&surface) {
                    rounded_clip(&cr, width as f64, height as f64, (4 * display_scale) as f64);
                    cr.set_source_pixbuf(&scaled, 0., 0.);
                    let _ = cr.paint();
                }
                // Keep physical pixels while GTK lays the image out in logical pixels.
                surface.set_device_scale(display_scale as f64, display_scale as f64);
                frame.add(&gtk::Image::from_surface(Some(&surface)));
            }
        }
    } else {
        frame.style_context().add_class("unavailable");
        let placeholder = label("Preview unavailable", "keytab-help");
        placeholder.set_xalign(0.5);
        placeholder.set_margin_top(8);
        placeholder.set_margin_bottom(8);
        frame.add(&placeholder);
    }
    frame
}

fn app_icon(app: &str, apps: &[gtk::gio::AppInfo]) -> gtk::Widget {
    let name = app.to_lowercase();
    let info = apps.iter().find(|info| {
        info.display_name().to_lowercase() == name || info.id().is_some_and(|id| id.to_lowercase().trim_end_matches(".desktop").rsplit('.').next() == Some(name.as_str()))
    });
    if let Some(icon) = info.and_then(|info| info.icon()) {
        let image = gtk::Image::from_gicon(&icon, gtk::IconSize::Menu);
        image.set_pixel_size(20);
        image.set_valign(gtk::Align::Center);
        image.upcast()
    } else {
        let initial = label(&app.chars().next().unwrap_or('?').to_ascii_uppercase().to_string(), "keytab-monogram");
        initial.set_xalign(0.5);
        initial.set_size_request(20, 20);
        initial.set_valign(gtk::Align::Center);
        initial.upcast()
    }
}

fn clean_title<'a>(title: &'a str, app: &str) -> &'a str {
    if title.eq_ignore_ascii_case(app) { return ""; }
    for separator in [" — ", " – ", " - "] {
        if let Some((prefix, suffix)) = title.rsplit_once(separator) {
            if suffix.eq_ignore_ascii_case(app) { return prefix; }
        }
    }
    title
}

fn footer(view: &View<'_>) -> String {
    if let Some(edit) = view.edit {
        let reset = if matches!((edit.scope, edit.kind), (Scope::App, crate::keys::Kind::App) | (Scope::Window, crate::keys::Kind::Window)) { " · ⌦ reset to automatic" } else { "" };
        return format!("{}{reset} · ⎋ back", if edit.error.is_some() { "⌫ change" } else { "↵ save" });
    }
    if view.closing.is_some() { return "Close selected window?  ⌦ close window · ⎋ cancel".into(); }
    let mut actions: Vec<String> = Vec::new();
    if view.search.is_some() { actions.push("Type to search".into()); }
    actions.extend(["↵ open".into(), "Ctrl current window".into()]);
    if !view.typed.is_empty() { actions.push("⌫ back".into()); }
    actions.push(if view.pinned { "Ctrl+P turn preview off" } else if view.preview { "Ctrl+P keep preview on" } else { "L⇧ preview" }.into());
    if view.typed.is_empty() { actions.push(if view.search.is_some() { "R⇧ hints" } else if view.preview { "R⇧ search" } else { "R⇧ or / search" }.into()); }
    if !view.preview && view.typed.is_empty() { actions.push("⧵ set hint · Alt+H hide · ⌦ close window".into()); }
    actions.push("⎋ dismiss".into());
    actions.join(" · ")
}

fn render_legend(parent: &gtk::Box, text: &str, available: i32) {
    let foot = gtk::Box::new(gtk::Orientation::Vertical, 6);
    foot.style_context().add_class("keytab-footer");
    parent.pack_start(&foot, false, false, 0);
    let mut row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    foot.pack_start(&row, false, false, 0);
    let mut used = 0;
    for action in text.split(" · ") {
        let group = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for token in action.split_whitespace() {
            let keycap = ["↵", "Ctrl", "Ctrl+P", "Alt+H", "L⇧", "R⇧", "⌫", "⌦", "⎋", "⧵", "/"].contains(&token);
            let item = label(token, if keycap { "keytab-keycap" } else { "keytab-help" });
            item.set_valign(gtk::Align::Center);
            if keycap { item.set_xalign(0.5); }
            group.pack_start(&item, false, false, 0);
        }
        row.pack_start(&group, false, false, 0);
        group.show_all();
        let width = group.preferred_width().1;
        if used > 0 && used + 10 + width > available {
            row.remove(&group);
            row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            foot.pack_start(&row, false, false, 0);
            row.pack_start(&group, false, false, 0);
            used = width;
        } else {
            used += if used == 0 { width } else { 10 + width };
        }
    }
}

fn capture(ids: Vec<WinId>, cancel: Arc<AtomicBool>, display_scale: i32) {
    std::thread::spawn(move || {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let child = Command::new("/usr/bin/python3")
            .arg("-c").arg(include_str!("../../scripts/thumbnail.py"))
            .arg(display_scale.to_string())
            .args(ids.iter().map(ToString::to_string)).stdout(Stdio::piped()).spawn();
        let Ok(mut child) = child else { eprintln!("keytab: couldn't start preview capture"); return };
        let mut output = child.stdout.take().unwrap();
        let mut pending: BTreeSet<_> = ids.into_iter().collect();
        while !pending.is_empty() {
            let mut header = [0u8; 20];
            if output.read_exact(&mut header).is_err() { break; }
            let id = i64::from_le_bytes(header[..8].try_into().unwrap());
            let width = u32::from_le_bytes(header[8..12].try_into().unwrap()) as i32;
            let height = u32::from_le_bytes(header[12..16].try_into().unwrap()) as i32;
            let len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
            if !pending.remove(&id) { break; }
            if len == 0 { continue; }
            if !(1..=560 * display_scale).contains(&width) || !(1..=240 * display_scale).contains(&height) || len != (width * height * 4) as usize { break; }
            let mut data = vec![0; len];
            if output.read_exact(&mut data).is_err() { break; }
            let cancel = cancel.clone();
            gtk::glib::idle_add_once(move || {
                UI.with(|ui| {
                    if let Some(ui) = ui.borrow_mut().as_mut() {
                        let bytes = gtk::glib::Bytes::from_owned(data);
                        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(&bytes, gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, width, height, width * 4);
                        ui.previews.insert(id, pixbuf);
                    }
                });
                if !cancel.load(Ordering::Relaxed) { switcher::refresh(); }
            });
        }
        drop(output);
        let _ = child.wait();
        gtk::glib::idle_add_once(|| {
            UI.with(|ui| { if let Some(ui) = ui.borrow_mut().as_mut() { ui.capture_running = false; } });
            switcher::refresh();
        });
    });
}

pub(crate) fn render(view: &View<'_>) {
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        let Some(ui) = ui.as_mut() else { return };
        if !view.rows.is_empty() && !(view.preview || view.pinned) && view.search.is_none() && view.edit.is_none() {
            ui.previews.retain(|id, _| view.rows.iter().any(|row| row.win.id == *id));
            ui.attempted.retain(|id, _| view.rows.iter().any(|row| row.win.id == *id));
        }
        let preview = (view.preview || view.pinned) && view.edit.is_none() && (!view.rows.is_empty() || view.search.is_some());
        if !preview {
            if let Some(cancel) = ui.capture.take() { cancel.store(true, Ordering::Relaxed); }
        } else if !ui.capture_running {
            let cancel = Arc::new(AtomicBool::new(false));
            ui.capture = Some(cancel.clone());
            let ids = view.selected.into_iter().filter(|id| ui.attempted.get(id).is_none_or(|when| when.elapsed() > Duration::from_secs(5))).collect::<Vec<_>>();
            for &id in &ids { ui.attempted.insert(id, Instant::now()); }
            if !ids.is_empty() { ui.capture_running = true; capture(ids, cancel, ui.window.scale_factor().clamp(1, 4)); }
        }
        ui.searching = view.search.is_some() && view.edit.is_none();
        for child in ui.content.children() {
            if !(ui.searching && child == ui.search_header.clone().upcast::<gtk::Widget>()) {
                ui.content.remove(&child);
            }
        }
        if view.rows.is_empty() && view.search.is_none() && view.edit.is_none() {
            ui.card.hide();
            return;
        }
        let mut query_entry = None;
        if let Some(edit) = view.edit {
            let dialog = gtk::Box::new(gtk::Orientation::Vertical, 14);
            dialog.set_margin_top(28);
            dialog.set_margin_bottom(28);
            dialog.set_margin_start(20);
            dialog.set_margin_end(20);
            let scope = if edit.scope == Scope::App { "Hint for every window of this app · saved for later windows" } else { "Hint for this window" };
            dialog.pack_start(&label(scope, "keytab-help"), false, false, 0);
            let target = if edit.scope == Scope::App || edit.title == edit.app { edit.app.clone() } else { format!("{}  {}", edit.app, edit.title) };
            dialog.pack_start(&label(&target, "keytab-app"), false, false, 0);
            let slots = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            slots.set_halign(gtk::Align::Center);
            for i in 0..4.min(2.max(edit.keys.len() + 1)) {
                let slot = label(&edit.keys.chars().nth(i).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default(), "keytab-slot");
                slot.set_size_request(44, 52);
                slot.set_xalign(0.5);
                if i < edit.keys.len() { slot.style_context().add_class("on"); }
                slots.pack_start(&slot, false, false, 0);
            }
            dialog.pack_start(&slots, false, false, 0);
            dialog.pack_start(&label(edit.error.as_deref().unwrap_or(" "), "keytab-error"), false, false, 0);
            dialog.pack_start(&label(&format!("Current hint: {}", edit.current.to_uppercase()), "keytab-help"), false, false, 0);
            ui.content.pack_start(&dialog, false, false, 0);
        } else {
            if let Some(search) = &view.search {
                if ui.entry.text().as_str() != search.query {
                    ui.updating_query.set(true);
                    ui.entry.set_text(search.query);
                    ui.entry.set_position(-1);
                    ui.updating_query.set(false);
                }
                if ui.search_header.parent().is_none() {
                    ui.content.pack_start(&ui.search_header, false, false, 0);
                }
                ui.content.reorder_child(&ui.search_header, 0);
                query_entry = Some(ui.entry.clone());
            }
            let list = gtk::ListBox::new();
            list.style_context().add_class("keytab-list");
            list.set_margin_start(6);
            list.set_margin_end(6);
            list.set_margin_top(6);
            list.set_margin_bottom(6);
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.set_activate_on_single_click(true);
            let ids: Vec<_> = view.rows.iter().map(|row| row.win.id).collect();
            list.connect_row_activated(move |_, row| {
                if let Some(&id) = ids.get(row.index() as usize) { switcher::activate(id); }
            });
            for row in view.rows.iter().filter(|row| view.search.is_some() || row.win.key.starts_with(view.typed)) {
                let win = row.win;
                let item = gtk::ListBoxRow::new();
                item.style_context().add_class("keytab-row");
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let hint = label(&win.key.to_uppercase(), "keytab-hint");
                hint.set_markup(&hint_markup(&win.key, view.typed));
                hint.set_width_chars(4);
                hint.set_xalign(0.5);
                hint.set_valign(gtk::Align::Center);
                match win.kind {
                    crate::keys::Kind::Window => hint.style_context().add_class("window"),
                    crate::keys::Kind::App => hint.style_context().add_class("app"),
                    _ => {}
                }
                if view.closing == Some(win.id) { item.style_context().add_class("closing"); }
                if view.search.is_none() { line.pack_start(&hint, false, false, 0); }
                line.pack_start(&app_icon(&win.app, &ui.apps), false, false, 0);
                let app = label("", "keytab-app");
                app.set_markup(&highlighted(&win.app, row.hits, 0));
                app.set_max_width_chars(16);
                line.pack_start(&app, false, false, 0);
                let title = label("", "keytab-title");
                title.set_markup(&highlighted(clean_title(&win.title, &win.app), row.hits, win.app.chars().count() + 1));
                line.pack_start(&title, true, true, 0);
                item.add(&line);
                list.add(&item);
                if view.selected == Some(win.id) { list.select_row(Some(&item)); }
            }
            if view.rows.is_empty() {
                let empty = label("No matching windows", "keytab-help");
                empty.set_xalign(0.5);
                empty.set_margin_start(16);
                empty.set_margin_end(16);
                empty.set_margin_top(18);
                empty.set_margin_bottom(18);
                ui.content.pack_start(&empty, false, false, 0);
            } else {
                let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
                scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
                scroll.set_propagate_natural_height(true);
                // Whole 40px rows, including the list's 6px top/bottom margins.
                scroll.set_max_content_height(if preview { 6 * 40 + 12 } else { 12 * 40 + 12 });
                scroll.add(&list);
                let adjustment = scroll.vadjustment();
                list.connect_size_allocate(move |list, _| {
                    if let Some(row) = list.selected_row() {
                        if let Some((_, y)) = row.translate_coordinates(list, 0, 0) {
                            let top = (y + list.margin_top()) as f64;
                            adjustment.clamp_page(top, top + row.allocated_height() as f64);
                            adjustment.set_value((adjustment.value() / 40.).round() * 40.);
                        }
                    }
                });
                ui.content.pack_start(&scroll, true, true, 0);
            }
            if preview {
                if let Some(id) = view.selected {
                    ui.content.pack_start(&thumbnail(ui.previews.get(&id), ui.window.scale_factor().clamp(1, 4)), false, false, 0);
                }
            }
        }
        let card_width = ui.window.allocated_width() - ui.card.margin_start() - ui.card.margin_end();
        render_legend(&ui.content, &footer(view), card_width - 34);
        for child in ui.content.children() { child.show_all(); }
        ui.content.show();
        ui.card.show();
        if let Some(entry) = query_entry {
            if !entry.has_focus() { entry.grab_focus_without_selecting(); }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{highlighted, hint_markup, clean_title, footer, preview_pixels, native_search_key, rounded_clip};

    #[test]
    fn rounded_preview_corners_are_transparent_without_removing_the_center() {
        let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 20, 20).unwrap();
        {
            let cr = gtk::cairo::Context::new(&surface).unwrap();
            rounded_clip(&cr, 20., 20., 6.);
            cr.set_source_rgba(1., 0., 0., 1.);
            cr.paint().unwrap();
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let alpha = |x: usize, y: usize| u32::from_ne_bytes(data[y * stride + x * 4..y * stride + x * 4 + 4].try_into().unwrap()) >> 24;
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(19, 19), 0);
        assert_eq!(alpha(10, 10), 255);
    }

    #[test]
    fn search_editing_uses_native_bindings_but_keeps_switcher_commands() {
        use gtk::gdk::ModifierType as M;
        use crate::switcher::Key;
        assert!(native_search_key(Key::Back, M::CONTROL_MASK));
        assert!(native_search_key(Key::Delete, M::CONTROL_MASK));
        for c in ['a', 'c', 'x', 'v'] { assert!(native_search_key(Key::Char(c), M::CONTROL_MASK)); }
        assert!(native_search_key(Key::Other, M::SHIFT_MASK)); // Shift+arrow selection.
        assert!(!native_search_key(Key::Char('p'), M::CONTROL_MASK));
        assert!(!native_search_key(Key::HintWindow, M::CONTROL_MASK));
        assert!(!native_search_key(Key::HintApp, M::CONTROL_MASK | M::SHIFT_MASK));
        assert!(!native_search_key(Key::Down, M::empty()));
        assert!(!native_search_key(Key::Enter, M::CONTROL_MASK));
    }

    #[test]
    fn preview_uses_physical_pixels_without_growing_the_logical_layout() {
        assert_eq!(preview_pixels(1120, 640, 1), (420, 240));
        assert_eq!(preview_pixels(1120, 640, 2), (840, 480));
        assert_eq!(preview_pixels(480, 285, 2), (480, 285), "small sources must not be upscaled");
        assert_eq!(preview_pixels(1, 10000, 2), (1, 480));
    }

    #[test]
    fn titles_keep_their_prefix_and_remove_only_a_redundant_app_suffix() {
        assert_eq!(clean_title("Downloads — Floorp", "Floorp"), "Downloads");
        assert_eq!(clean_title("FLOORP", "Floorp"), "");
        assert_eq!(clean_title("Article — Publisher", "Floorp"), "Article — Publisher");
    }

    #[test]
    fn footer_follows_hint_progress_pinning_and_close_confirmation() {
        let mut win = crate::keys::Win::new(1, "Floorp".into(), "Page".into());
        win.key = "b2".into();
        let mut view = crate::switcher::View { rows: vec![crate::switcher::Row { win: &win, hits: &[] }], typed: "b", search: None, edit: None, dom_input: true, preview: true, pinned: false, selected: Some(1), closing: None };
        assert!(!footer(&view).contains("to finish"));
        assert!(footer(&view).contains("Ctrl+P keep preview on"));
        assert!(!footer(&view).contains("set hint"));
        view.pinned = true;
        assert!(footer(&view).contains("turn preview off"));
        view.closing = Some(1);
        assert!(footer(&view).contains("⎋ cancel"));
        assert!(!footer(&view).contains("↵ open"));
    }

    #[test]
    fn partial_hints_fade_only_the_typed_letters() {
        assert_eq!(hint_markup("b2", ""), "B2");
        assert_eq!(hint_markup("b2", "b"), "<span alpha='72%'>B</span>2");
        assert_eq!(hint_markup("b2", "b2"), "<span alpha='72%'>B</span><span alpha='72%'>2</span>");
    }

    #[test]
    fn highlighting_escapes_window_titles_and_uses_character_positions() {
        assert_eq!(highlighted("<&", &[], 0), "&lt;&amp;");
        assert_eq!(highlighted("é<&", &[4], 3), "é<span foreground='#8ab4ff' weight='bold' underline='single'>&lt;</span>&amp;");
    }
}
