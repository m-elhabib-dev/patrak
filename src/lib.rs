pub mod config;
pub mod db;
pub mod launch;
pub mod library;
pub mod progress;
pub mod setup;
pub mod sources;
pub mod sync;

use anyhow::Result;
use config::{Config, Paths};

/// Everything a caller needs to show current progress: config, a database connection and a
/// freshly synced snapshot.
pub struct Session {
    pub paths: Paths,
    pub cfg: Config,
    pub conn: rusqlite::Connection,
}

impl Session {
    pub fn open() -> Result<Self> {
        let paths = Paths::from_env();
        let cfg = Config::load(&paths.config)?;
        let conn = db::open(&paths.db())?;
        Ok(Self { paths, cfg, conn })
    }

    pub fn sync(&mut self) -> Result<sync::Report> {
        sync::sync(&mut self.conn, &self.cfg, &self.paths)
    }

    pub fn snapshot(&self) -> Result<progress::Snapshot> {
        progress::build(&self.conn, &self.cfg)
    }
}
