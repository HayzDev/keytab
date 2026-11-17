//! Native Linux renderer; the switcher owns all input and hint/search behavior.
use crate::switcher::{self, Key, Scope, View};
use gtk::prelude::*;
use std::cell::RefCell;
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
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

const CSS: &str = r#"
window#keytab { background-color: transparent; }
#keytab .keytab-card {
    background-color: rgba(22, 22, 26, 0.84);
    border: 1px solid rgba(120, 140, 170, 0.35); border-radius: 10px;
    color: #ececf1; font: 14px sans-serif;
}
#keytab .keytab-card > border { border: none; }
#keytab .keytab-list, #keytab scrolledwindow, #keytab viewport { background: transparent; border: none; }
#keytab .keytab-row { padding: 8px 10px; border-radius: 9px; background: transparent; }
#keytab .keytab-row:hover { background: rgba(255,255,255,0.05); }
#keytab .keytab-row:selected { background: rgba(138,180,255,0.14); }
#keytab .keytab-app { font-weight: 600; }
#keytab .keytab-title, #keytab .keytab-help { color: #aaaab7; }
#keytab .keytab-hint { background: rgba(255,255,255,0.06); border-radius: 6px; padding: 3px 4px; font: bold 13px monospace; }
#keytab .keytab-hint.window { background: transparent; border: 1px solid #8ab4ff; color: #8ab4ff; }
#keytab .keytab-hint.app { background: rgba(138,180,255,0.22); color: #8ab4ff; }
#keytab .keytab-query { font-size: 16px; padding: 14px 16px; }
#keytab entry { background: transparent; border: none; box-shadow: none; color: #ececf1; padding: 0; min-height: 24px; }
#keytab .keytab-slot { background: rgba(255,255,255,0.06); border-radius: 10px; font: 24px monospace; }
#keytab .keytab-slot.on { border: 1px solid #8ab4ff; color: #8ab4ff; }
#keytab .keytab-error { color: #ff7b7b; font-size: 13px; }
#keytab .keytab-help { font-size: 12px; }
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
    card.set_valign(gtk::Align::Center);
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
    window.connect_key_press_event(|_, event| {
        if !matches!(key(event), Key::Preview | Key::Search) && event.state().contains(gtk::gdk::ModifierType::SHIFT_MASK) {
            switcher::on_key(Key::Preview, true);
        }
        switcher::on_key(key(event), true);
        gtk::glib::Propagation::Stop
    });
    window.connect_key_release_event(|_, event| {
        switcher::on_key(key(event), false);
        gtk::glib::Propagation::Stop
    });
    UI.with(|ui| *ui.borrow_mut() = Some(Ui { window: window.clone(), card, content, previews: BTreeMap::new(), capture: None, capture_running: false, attempted: BTreeMap::new() }));
}

pub(crate) fn window() -> gtk::Window {
    UI.with(|ui| ui.borrow().as_ref().expect("native overlay").window.clone())
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
            out.push_str(if next { "<span foreground='#8ab4ff' weight='bold'>" } else { "</span>" });
            start = byte;
            marked = next;
        }
    }
    out.push_str(&gtk::glib::markup_escape_text(&text[start..]));
    if marked { out.push_str("</span>"); }
    out
}

fn hint_markup(hint: &str, typed: &str) -> String {
    if typed.is_empty() || !hint.starts_with(typed) {
        return gtk::glib::markup_escape_text(&hint.to_uppercase()).to_string();
    }
    hint.chars().enumerate().map(|(i, c)| {
        let style = if i < typed.chars().count() {
            "foreground='#777785'"
        } else if i == typed.chars().count() {
            "foreground='#8ab4ff' underline='single'"
        } else {
            "foreground='#aaaab7'"
        };
        format!("<span {style}>{}</span>", gtk::glib::markup_escape_text(&c.to_ascii_uppercase().to_string()))
    }).collect()
}

fn thumbnail(pixbuf: Option<&gtk::gdk_pixbuf::Pixbuf>, large: bool) -> gtk::Box {
    let (width, height) = if large { (480, 240) } else { (104, 66) };
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.set_size_request(width, height);
    frame.set_halign(gtk::Align::Center);
    if let Some(pixbuf) = pixbuf {
        let scale = (width as f64 / pixbuf.width() as f64).min(height as f64 / pixbuf.height() as f64);
        if let Some(scaled) = pixbuf.scale_simple((pixbuf.width() as f64 * scale) as i32, (pixbuf.height() as f64 * scale) as i32, gtk::gdk_pixbuf::InterpType::Bilinear) {
            let image = gtk::Image::from_pixbuf(Some(&scaled));
            frame.pack_start(&image, true, true, 0);
        }
    } else {
        frame.pack_start(&label("Preview unavailable", "keytab-help"), true, true, 0);
    }
    frame
}

fn capture(ids: Vec<WinId>, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let child = Command::new("/usr/bin/python3")
            .arg("-c").arg(include_str!("../../scripts/thumbnail.py"))
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
            if !(1..=480).contains(&width) || !(1..=300).contains(&height) || len != (width * height * 4) as usize { break; }
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
        } else if ui.capture.is_none() && !ui.capture_running {
            let cancel = Arc::new(AtomicBool::new(false));
            ui.capture = Some(cancel.clone());
            let ids = view.rows.iter().map(|row| row.win.id).filter(|id| ui.attempted.get(id).is_none_or(|when| when.elapsed() > Duration::from_secs(5))).collect::<Vec<_>>();
            for &id in &ids { ui.attempted.insert(id, Instant::now()); }
            if !ids.is_empty() { ui.capture_running = true; capture(ids, cancel); }
        }
        for child in ui.content.children() { ui.content.remove(&child); }
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
            let scope = if edit.scope == Scope::App { "Hint for every window of this app · saved" } else { "Hint for this window" };
            dialog.pack_start(&label(scope, "keytab-help"), false, false, 0);
            let target = if edit.scope == Scope::App || edit.title == edit.app { edit.app.clone() } else { format!("{}  {}", edit.app, edit.title) };
            dialog.pack_start(&label(&target, "keytab-app"), false, false, 0);
            let slots = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            slots.set_halign(gtk::Align::Center);
            for i in 0..4 {
                let slot = label(&edit.keys.chars().nth(i).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default(), "keytab-slot");
                slot.set_size_request(44, 52);
                slot.set_xalign(0.5);
                if i < edit.keys.len() { slot.style_context().add_class("on"); }
                slots.pack_start(&slot, false, false, 0);
            }
            dialog.pack_start(&slots, false, false, 0);
            dialog.pack_start(&label(edit.error.as_deref().unwrap_or(" "), "keytab-error"), false, false, 0);
            dialog.pack_start(&label(&format!("Now {} · Enter save · Delete reset · Esc back", edit.current.to_uppercase()), "keytab-help"), false, false, 0);
            ui.content.pack_start(&dialog, false, false, 0);
        } else {
            if let Some(search) = &view.search {
                let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                header.style_context().add_class("keytab-query");
                header.pack_start(&gtk::Image::from_icon_name(Some("system-search-symbolic"), gtk::IconSize::Menu), false, false, 0);
                let entry = gtk::Entry::new();
                entry.set_has_frame(false);
                entry.set_placeholder_text(Some("Search windows"));
                entry.set_text(search.query);
                entry.set_position(-1);
                header.pack_start(&entry, true, true, 0);
                ui.content.pack_start(&header, false, false, 0);
                query_entry = Some(entry);
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
            for row in &view.rows {
                let win = row.win;
                let item = gtk::ListBoxRow::new();
                item.style_context().add_class("keytab-row");
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let large = preview && view.search.is_none() && !view.typed.is_empty() && view.selected == Some(win.id);
                let thumbnail = preview.then(|| thumbnail(ui.previews.get(&win.id), large));
                if !large {
                    if let Some(image) = &thumbnail { line.pack_start(image, false, false, 0); }
                }
                if view.search.is_none() {
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
                    if !win.key.starts_with(view.typed) { item.set_opacity(0.25); }
                    line.pack_start(&hint, false, false, 0);
                }
                let app = label("", "keytab-app");
                app.set_markup(&highlighted(&win.app, row.hits, 0));
                app.set_max_width_chars(16);
                line.pack_start(&app, false, false, 0);
                let title = label("", "keytab-title");
                if win.title != win.app {
                    title.set_markup(&highlighted(&win.title, row.hits, win.app.chars().count() + 1));
                }
                line.pack_start(&title, true, true, 0);
                let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
                body.pack_start(&line, false, false, 0);
                if large {
                    if let Some(image) = thumbnail { body.pack_start(&image, false, false, 0); }
                }
                item.add(&body);
                list.add(&item);
                if view.selected == Some(win.id) { list.select_row(Some(&item)); }
            }
            if view.rows.is_empty() {
                let empty = label("No matching windows", "keytab-help");
                empty.set_margin_start(16);
                empty.set_margin_bottom(16);
                ui.content.pack_start(&empty, false, false, 0);
            } else {
                let scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
                scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
                scroll.set_propagate_natural_height(true);
                scroll.set_max_content_height(600);
                scroll.add(&list);
                let adjustment = scroll.vadjustment();
                list.connect_size_allocate(move |list, _| {
                    if let Some(row) = list.selected_row() {
                        let rect = row.allocation();
                        adjustment.clamp_page(rect.y() as f64, (rect.y() + rect.height()) as f64);
                    }
                });
                ui.content.pack_start(&scroll, true, true, 0);
            }
        }
        for child in ui.content.children() { child.show_all(); }
        ui.content.show();
        ui.card.show();
        if let Some(entry) = query_entry {
            entry.grab_focus_without_selecting();
            entry.set_position(-1);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{highlighted, hint_markup};

    #[test]
    fn partial_hints_fade_typed_letters_and_highlight_the_next_key() {
        assert_eq!(hint_markup("b2", ""), "B2");
        assert_eq!(hint_markup("b2", "b"), "<span foreground='#777785'>B</span><span foreground='#8ab4ff' underline='single'>2</span>");
        assert_eq!(hint_markup("b2", "b2"), "<span foreground='#777785'>B</span><span foreground='#777785'>2</span>");
    }

    #[test]
    fn highlighting_escapes_window_titles_and_uses_character_positions() {
        assert_eq!(highlighted("<&", &[], 0), "&lt;&amp;");
        assert_eq!(highlighted("é<&", &[4], 3), "é<span foreground='#8ab4ff' weight='bold'>&lt;</span>&amp;");
    }
}
