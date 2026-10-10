use super::App;
use super::widgets::label;
use adw::prelude::*;
use chrono::NaiveDate;
use gtk::{gdk, glib};
use patrak::db;
use patrak::review::{self, Card, Grade};
use rusqlite::Connection;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

/// Reviews the cards due today. Space shows the answer; 1–4 grade it.
pub fn show(app: &Rc<App>) {
    let today = app.cfg.borrow().today();
    let loaded = db::open(&app.paths.db()).and_then(|conn| {
        let cards = review::due(&conn, today)?;
        Ok((conn, cards))
    });
    let (conn, cards) = match loaded {
        Ok(v) => v,
        Err(e) => return app.toast(&format!("Couldn't load cards: {e:#}")),
    };
    if cards.is_empty() {
        return app.toast("No cards due. Write Q:/A: lines in your notes to make some.");
    }

    let title = adw::WindowTitle::new("Review", "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let source = label("", &["caption", "dim-label"]);
    let question = label("", &["title-2"]);
    let answer = label("", &[]);
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    let card_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    for w in [
        source.upcast_ref::<gtk::Widget>(),
        question.upcast_ref(),
        separator.upcast_ref(),
        answer.upcast_ref(),
    ] {
        card_box.append(w);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&card_box)
        .build();

    let reveal = gtk::Button::builder()
        .label("Show Answer")
        .tooltip_text("Space")
        .halign(gtk::Align::Center)
        .build();
    reveal.add_css_class("pill");
    reveal.add_css_class("suggested-action");
    let grades = gtk::Box::builder().spacing(8).homogeneous(true).build();
    let mut previews = Vec::new();
    for (i, grade) in Grade::ALL.into_iter().enumerate() {
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
        inner.append(&gtk::Label::new(Some(grade.label())));
        let preview = gtk::Label::new(None);
        preview.add_css_class("caption");
        preview.add_css_class("dim-label");
        inner.append(&preview);
        let button = gtk::Button::builder()
            .child(&inner)
            .tooltip_text(format!("Key {}", i + 1))
            .build();
        if grade == Grade::Again {
            button.add_css_class("destructive-action");
        }
        grades.append(&button);
        previews.push((grade, button, preview));
    }
    let controls = gtk::Stack::builder()
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    controls.add_named(&reveal, Some("reveal"));
    controls.add_named(&grades, Some("grades"));

    let card_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card_page.append(&scroller);
    card_page.append(&controls);
    let done_page = adw::StatusPage::builder()
        .icon_name("object-select-symbolic")
        .title("All Done")
        .build();
    let pages = gtk::Stack::new();
    pages.add_named(&card_page, Some("card"));
    pages.add_named(&done_page, Some("done"));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&pages));
    let dialog = adw::Dialog::builder()
        .title("Review")
        .content_width(640)
        .content_height(520)
        .child(&toolbar)
        .build();

    let total = cards.len();
    let session = Rc::new(Session {
        conn,
        today,
        queue: RefCell::new(cards.into()),
        total,
        finished: Cell::new(0),
        shown_at: Cell::new(Instant::now()),
        revealed: Cell::new(false),
        title,
        source,
        question,
        answer,
        separator,
        controls,
        pages,
        done_page,
        previews: previews.iter().map(|(g, _, p)| (*g, p.clone())).collect(),
    });
    session.show_current();

    let s = session.clone();
    reveal.connect_clicked(move |_| s.reveal());
    for (grade, button, _) in previews {
        let s = session.clone();
        button.connect_clicked(move |_| s.grade(grade));
    }

    let keys = gtk::EventControllerKey::new();
    let s = session.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if matches!(key, gdk::Key::space | gdk::Key::Return | gdk::Key::KP_Enter)
            && !s.revealed.get()
        {
            s.reveal();
            return glib::Propagation::Stop;
        }
        match key.to_unicode().and_then(Grade::from_key) {
            Some(grade) if s.revealed.get() => {
                s.grade(grade);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    dialog.add_controller(keys);

    let weak = Rc::downgrade(app);
    dialog.connect_closed(move |_| {
        if let Some(app) = weak.upgrade() {
            app.refresh();
        }
    });
    dialog.present(Some(&app.window));
}

struct Session {
    conn: Connection,
    today: NaiveDate,
    queue: RefCell<VecDeque<Card>>,
    total: usize,
    finished: Cell<usize>,
    shown_at: Cell<Instant>,
    revealed: Cell<bool>,
    title: adw::WindowTitle,
    source: gtk::Label,
    question: gtk::Label,
    answer: gtk::Label,
    separator: gtk::Separator,
    controls: gtk::Stack,
    pages: gtk::Stack,
    done_page: adw::StatusPage,
    previews: Vec<(Grade, gtk::Label)>,
}

impl Session {
    fn show_current(&self) {
        let queue = self.queue.borrow();
        let Some(card) = queue.front() else {
            let n = self.finished.get();
            self.done_page.set_description(Some(&format!(
                "Reviewed {n} card{}.",
                if n == 1 { "" } else { "s" }
            )));
            self.title.set_subtitle("");
            self.pages.set_visible_child_name("done");
            return;
        };
        self.title.set_subtitle(&format!(
            "{} of {}",
            (self.finished.get() + 1).min(self.total),
            self.total
        ));
        self.source.set_label(&card.source());
        self.question.set_label(&card.question);
        self.answer.set_label(&card.answer);
        self.answer.set_visible(false);
        self.separator.set_visible(false);
        for (grade, preview) in &self.previews {
            preview.set_label(&card.preview(*grade));
        }
        self.controls.set_visible_child_name("reveal");
        self.revealed.set(false);
        self.shown_at.set(Instant::now());
    }

    fn reveal(&self) {
        if self.queue.borrow().is_empty() {
            return;
        }
        self.answer.set_visible(true);
        self.separator.set_visible(true);
        self.controls.set_visible_child_name("grades");
        self.revealed.set(true);
    }

    fn grade(&self, grade: Grade) {
        let Some(card) = self.queue.borrow_mut().pop_front() else {
            return;
        };
        let seconds = self.shown_at.get().elapsed().as_secs_f64();
        match review::grade(&self.conn, &card, grade, self.today, seconds) {
            Ok(next) if grade == Grade::Again => self.queue.borrow_mut().push_back(next),
            Ok(_) => self.finished.set(self.finished.get() + 1),
            Err(e) => {
                eprintln!("saving review: {e:#}");
                self.queue.borrow_mut().push_front(card);
            }
        }
        self.show_current();
    }
}
