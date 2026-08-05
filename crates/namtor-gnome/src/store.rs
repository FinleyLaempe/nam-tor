// SPDX-License-Identifier: GPL-3.0-only
//! Store access wrapper for the GNOME UI.

use namtor_store::{StoreError, TemplateStore};
use std::path::Path;

pub struct UiStore {
    store: TemplateStore,
}

pub struct EnrolledUser {
    pub uid: u32,
    pub username: String,
    pub num_templates: u32,
    pub model_id: String,
}

impl UiStore {
    pub fn open(dir: &Path, key: &Path) -> Result<Self, StoreError> {
        let store = TemplateStore::open(dir, key)?;
        Ok(Self { store })
    }

    pub fn list(&self) -> Result<Vec<EnrolledUser>, StoreError> {
        let uids = self.store.list_uids()?;
        let mut users = Vec::new();
        for uid in uids {
            if let Ok(record) = self.store.load(uid) {
                users.push(EnrolledUser {
                    uid: record.meta.uid,
                    username: record.meta.username.clone(),
                    num_templates: record.meta.num_templates,
                    model_id: record.meta.model_id.clone(),
                });
            }
        }
        Ok(users)
    }

    pub fn delete(&self, uid: u32) -> Result<(), StoreError> {
        self.store.delete(uid)
    }
}
