//! Where a setting lives: in the global settings (shared by every project) or in one project.
//! Variables, secrets, injections, images and the allowed hosts exist in both; a run sees the
//! two together, and what the project defines wins over the global one with the same name.

use std::path::PathBuf;

use serde::Serialize;

use crate::config::projects;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Scope(Option<String>);

impl Scope {
    pub fn global() -> Scope {
        Scope(None)
    }

    pub fn project(id: &str) -> Scope {
        Scope(Some(id.to_string()))
    }

    /// The scope the UI named: no project means the global settings.
    pub fn of(project: Option<String>) -> Result<Scope, String> {
        match project {
            None => Ok(Scope::global()),
            Some(id) if projects::valid_id(&id) => Ok(Scope(Some(id))),
            Some(_) => Err("bad project id".into()),
        }
    }

    /// The project's id; `None` for the global settings.
    pub fn id(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub fn is_global(&self) -> bool {
        self.0.is_none()
    }

    /// The directory the scope's files are in: the app data dir for the global settings,
    /// `<app data>/projects/<id>` for a project (which must exist).
    pub fn dir(&self, app: &tauri::AppHandle) -> Result<PathBuf, String> {
        let data = crate::util::data_dir(app)?;
        match &self.0 {
            None => Ok(data),
            Some(id) => {
                let dir = projects::dir_in(&data, id);
                if !dir.join(projects::FILE).is_file() {
                    return Err("no such project".into());
                }
                Ok(dir)
            }
        }
    }

    /// The name a secret has in the OS credential store.
    pub fn secret_account(&self, name: &str) -> String {
        match &self.0 {
            None => format!("secret:{name}"),
            Some(p) => format!("secret:{p}:{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_name_their_secrets_apart() {
        assert_eq!(Scope::global().secret_account("A"), "secret:A");
        assert_eq!(Scope::project("demo").secret_account("A"), "secret:demo:A");
        assert!(Scope::of(Some("../x".into())).is_err());
        assert!(Scope::of(None).unwrap().is_global());
        assert_eq!(Scope::of(Some("p-1".into())).unwrap().id(), Some("p-1"));
    }
}
