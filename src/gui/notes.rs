use super::widgets::label;
use super::{Action, App};
use adw::prelude::*;
use gtk::{gdk, glib, pango};
use patrak::db::{Item, Kind};
use patrak::launch::{self, Stamp};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// A markdown note for a lecture or book, saved half a second after typing stops.
pub fn show(app: &Rc<App>, item: Item) {
    let path = match launch::ensure_note(&item) {
        Ok(p) => p,
        Err(e) => return app.toast(&format!("Couldn't create the note: {e}")),
    };
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&std::fs::read_to_string(&path).unwrap_or_default());
    buffer.place_cursor(&buffer.end_iter());
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .vexpand(true)
        .build();
    let scroller = gtk::ScrolledWindow::builder().child(&view).build();
    let stamps = StampLinks::new(&buffer, item.kind);
    stamps.attach(app, &view, item.id);

    let stamp = gtk::Button::builder()
        .icon_name("alarm-symbolic")
        .tooltip_text("Insert the current position, e.g. [12:30]")
        .build();
    let editor = gtk::Button::builder()
        .icon_name("utilities-terminal-symbolic")
        .tooltip_text("Open in $EDITOR")
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&stamp);
    header.pack_start(&editor);

    let path_label = label(
        &format!(
            "{}\nClick a {} stamp to jump there. Q: question / A: answer lines become review cards.",
            path.display(),
            if item.kind == Kind::Book {
                "[p. 42]"
            } else {
                "[12:30]"
            }
        ),
        &["caption", "dim-label"],
    );
    path_label.set_margin_start(12);
    path_label.set_margin_end(12);
    path_label.set_margin_top(6);
    path_label.set_margin_bottom(6);
    path_label.set_selectable(true);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    toolbar.add_bottom_bar(&path_label);

    let dialog = adw::Dialog::builder()
        .title(&item.title)
        .content_width(700)
        .content_height(620)
        .child(&toolbar)
        .build();

    let saver = Rc::new(Saver {
        buffer: buffer.clone(),
        path,
        pending: RefCell::new(None),
    });
    let s = saver.clone();
    buffer.connect_changed(move |_| {
        s.schedule();
        stamps.highlight();
    });

    let (weak_app, id) = (Rc::downgrade(app), item.id);
    let b = buffer.clone();
    stamp.connect_clicked(move |_| {
        // The snapshot is refreshed while mpv plays, so this is the live position.
        let Some(app) = weak_app.upgrade() else {
            return;
        };
        let latest = app
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.item(id).cloned());
        b.insert_at_cursor(&launch::position_stamp(latest.as_ref().unwrap_or(&item)));
    });

    let (weak_app, s) = (Rc::downgrade(app), saver.clone());
    editor.connect_clicked(move |_| {
        let Some(app) = weak_app.upgrade() else {
            return;
        };
        s.save_now();
        let args = launch::editor_in_terminal_args(&app.cfg.borrow(), &s.path);
        let argv: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        match gtk::gio::Subprocess::newv(&argv, gtk::gio::SubprocessFlags::NONE) {
            Ok(process) => {
                let s = s.clone();
                glib::spawn_future_local(async move {
                    let _ = process.wait_future().await;
                    s.reload();
                });
            }
            Err(e) => app.toast(&format!("Couldn't open the terminal: {}", e.message())),
        }
    });

    dialog.connect_closed(move |_| saver.save_now());
    dialog.present(Some(&app.window));
}

struct Saver {
    buffer: gtk::TextBuffer,
    path: PathBuf,
    pending: RefCell<Option<glib::SourceId>>,
}

impl Saver {
    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.pending.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if let Some(s) = weak.upgrade() {
                // The source has fired; forget it rather than removing it.
                s.pending.borrow_mut().take();
                s.write();
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    fn save_now(&self) {
        if let Some(id) = self.pending.take() {
            id.remove();
            self.write();
        }
    }

    fn write(&self) {
        let text = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false);
        if let Err(e) = std::fs::write(&self.path, text.as_str()) {
            eprintln!("saving {}: {e}", self.path.display());
        }
    }

    fn reload(&self) {
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            self.buffer.set_text(&text);
            // Reloading isn't an edit; don't write the same text back.
            if let Some(id) = self.pending.take() {
                id.remove();
            }
        }
    }
}

/// Underlines `[12:30]` / `[p. 42]` stamps and opens the lecture or book there when clicked.
#[derive(Clone)]
struct StampLinks {
    buffer: gtk::TextBuffer,
    tag: gtk::TextTag,
    kind: Kind,
}

impl StampLinks {
    fn new(buffer: &gtk::TextBuffer, kind: Kind) -> Self {
        let tag = gtk::TextTag::builder()
            .name("stamp")
            .underline(pango::Underline::Single)
            .build();
        tag.set_foreground_rgba(Some(&adw::StyleManager::default().accent_color_rgba()));
        buffer.tag_table().add(&tag);
        let links = Self {
            buffer: buffer.clone(),
            tag,
            kind,
        };
        links.highlight();
        links
    }

    fn stamps(&self) -> Vec<(usize, usize, Stamp)> {
        let text = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false);
        launch::find_stamps(&text)
            .into_iter()
            .filter(|s| s.2.fits(self.kind))
            .collect()
    }

    fn highlight(&self) {
        let b = &self.buffer;
        b.remove_tag(&self.tag, &b.start_iter(), &b.end_iter());
        for (start, end, _) in self.stamps() {
            b.apply_tag(
                &self.tag,
                &b.iter_at_offset(start as i32),
                &b.iter_at_offset(end as i32),
            );
        }
    }

    /// The stamp under widget coordinates (x, y), if any.
    fn at(&self, view: &gtk::TextView, x: f64, y: f64) -> Option<Stamp> {
        let (bx, by) =
            view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = view.iter_at_location(bx, by)?;
        if !iter.has_tag(&self.tag) {
            return None;
        }
        let offset = iter.offset() as usize;
        self.stamps()
            .into_iter()
            .find(|&(s, e, _)| s <= offset && offset < e)
            .map(|s| s.2)
    }

    fn attach(&self, app: &Rc<App>, view: &gtk::TextView, item_id: i64) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let (links, v, weak) = (self.clone(), view.clone(), Rc::downgrade(app));
        click.connect_released(move |_, presses, x, y| {
            // A drag selects text; only a plain single click follows the stamp.
            if presses != 1 || links.buffer.has_selection() {
                return;
            }
            let (Some(stamp), Some(app)) = (links.at(&v, x, y), weak.upgrade()) else {
                return;
            };
            app.handle(match stamp {
                Stamp::Time(secs) => Action::PlayAt { item_id, secs },
                Stamp::Page(page) => Action::ReadAt { item_id, page },
            });
        });
        view.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let (links, v) = (self.clone(), view.clone());
        motion.connect_motion(move |_, x, y| {
            let over = links.at(&v, x, y).is_some();
            v.set_cursor_from_name(Some(if over { "pointer" } else { "text" }));
        });
        view.add_controller(motion);
    }
}
