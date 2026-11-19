//! Explicit configuration values without changing the process environment.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

#[derive(Default)]
pub struct Environment(BTreeMap<OsString, OsString>);

impl Environment {
    pub fn set(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        self.0
            .insert(name.as_ref().to_owned(), value.as_ref().to_owned());
    }

    pub fn remove(&mut self, name: impl AsRef<OsStr>) {
        self.0.remove(name.as_ref());
    }

    pub fn get(&self, name: &str) -> Option<OsString> {
        self.0.get(OsStr::new(name)).cloned()
    }
}
