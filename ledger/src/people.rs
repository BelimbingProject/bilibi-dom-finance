//! People's names for Finance pages ("Approved by Ana Lim"), never "User #5".
//! Every Finance page that says who did something reads the names here, once
//! per render, through Base's principal directory: Core names each account
//! inside the reader's own tenant, and an account it cannot name (deleted, or
//! another tenant's) keeps its durable label ("User 5") so the row still reads.
use bilibi_principal_directory::{
    DirectoryClient, PrincipalDirectory, PrincipalKind, PrincipalRef, Query,
};
use bilibi_tenancy::Scope;
use std::collections::HashMap;

/// The registry service a page takes the directory from; its module declares
/// `base/principal_directory` among its dependencies.
pub const DIRECTORY: &str = "base/principal_directory.directory";

/// The names of the people one page mentions.
#[derive(Clone, Debug, Default)]
pub struct People {
    names: HashMap<i64, String>,
}

impl People {
    /// Names every user id in `ids` (`None`s and repeats are skipped). A
    /// directory that cannot answer leaves every name at its durable label:
    /// a page never fails because a name is missing.
    pub async fn read(
        directory: &PrincipalDirectory,
        client: &dyn DirectoryClient,
        scope: &Scope,
        ids: impl IntoIterator<Item = Option<i64>>,
    ) -> Self {
        let mut candidates: Vec<PrincipalRef> = ids
            .into_iter()
            .flatten()
            .filter_map(|id| PrincipalRef::new(PrincipalKind::User, id).ok())
            .collect();
        candidates.sort();
        candidates.dedup();
        if candidates.is_empty() {
            return Self::default();
        }
        let Ok(ranked) = directory
            .rank(client, scope, &candidates, &Query::default())
            .await
        else {
            return Self::default();
        };
        Self {
            names: ranked
                .named
                .into_iter()
                .map(|named| (named.principal.id(), named.name))
                .collect(),
        }
    }

    /// One person's name, or their durable label when nobody can name them.
    pub fn name(&self, user: i64) -> String {
        self.names.get(&user).cloned().unwrap_or_else(|| {
            PrincipalRef::new(PrincipalKind::User, user)
                .map_or_else(|_| "Someone".into(), |p| p.durable_label())
        })
    }

    /// Who did it: "Ana Lim", or "Ana Lim (impersonated by Ben Tan)" when
    /// someone acted as them; "—" when nobody is recorded.
    pub fn who(&self, user: Option<i64>, impersonator: Option<i64>) -> String {
        match (user, impersonator) {
            (None, _) => "—".into(),
            (Some(user), None) => self.name(user),
            (Some(user), Some(by)) => {
                format!("{} (impersonated by {})", self.name(user), self.name(by))
            }
        }
    }
}
