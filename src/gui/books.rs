use super::widgets::{
    done_check, icon_button, isolate, page, pill_button, progress, row, section, value_label,
};
use super::{Act, Action};
use adw::prelude::*;
use patrak::progress::{Snapshot, item_status};

pub struct BooksPage {
    pub root: gtk::Stack,
    content: gtk::Box,
    act: Act,
}

impl BooksPage {
    pub fn new(act: Act) -> Self {
        let (scroller, content) = page(860);
        let empty = adw::StatusPage::builder()
            .icon_name("accessories-dictionary-symbolic")
            .title("No Books Yet")
            .description("Put PDF, EPUB or DjVu files in one of your book folders (Preferences → Library). Pages you read in zathura are tracked automatically.")
            .build();
        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("books"));
        root.add_named(&empty, Some("empty"));
        root.set_visible_child_name("empty");
        Self { root, content, act }
    }

    pub fn update(&self, snap: &Snapshot) {
        super::widgets::clear(&self.content);
        self.root.set_visible_child_name(if snap.books.is_empty() {
            "empty"
        } else {
            "books"
        });

        let reading: Vec<_> = snap
            .books
            .iter()
            .filter(|b| !b.is_done() && b.position > 0.0)
            .collect();
        let unread: Vec<_> = snap
            .books
            .iter()
            .filter(|b| !b.is_done() && b.position <= 0.0)
            .collect();
        let finished: Vec<_> = snap.books.iter().filter(|b| b.is_done()).collect();
        for (title, books) in [
            ("Reading", reading),
            ("Not Started", unread),
            ("Finished", finished),
        ] {
            if books.is_empty() {
                continue;
            }
            let (section_box, list) = section(title);
            for book in books {
                let status = if book.missing {
                    format!("File missing  ·  {}", item_status(book))
                } else {
                    item_status(book)
                };
                let subtitle = if book.course.is_empty() {
                    status
                } else {
                    format!("{}  ·  {}", isolate(&book.course), isolate(&status))
                };
                let r = row(&book.title, &subtitle);
                if let Some(f) = book.fraction() {
                    r.add_suffix(&value_label(
                        &format!("{:.0}%", f * 100.0),
                        &["numeric", "dim-label"],
                    ));
                    r.add_suffix(&progress(f, 90));
                }
                let id = book.id;
                if !book.missing {
                    r.add_suffix(&icon_button(
                        "document-edit-symbolic",
                        "Notes",
                        &self.act,
                        move || Action::Note(id),
                    ));
                }
                r.add_suffix(&done_check(id, book.is_done(), &self.act));
                if !book.missing {
                    r.add_suffix(&pill_button("Read", &self.act, move || Action::Read(id)));
                }
                list.append(&r);
            }
            self.content.append(&section_box);
        }
    }
}
