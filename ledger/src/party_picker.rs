//! One shared Company/Person picker for Add customers and Add suppliers.
//! Every selection and creation reaches its Finance operation contract.
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Error, Result};
use bilibi_records::{Hit, Reference, Resolved};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    error_sentence, Button, ErrorSummary, Field, Html, InputKind, SearchMultiOption,
    SearchMultiSelect, StatusKind,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
const KINDS: [&str; 2] = ["company", "person"];
const NEW_FIELDS: &[(&str, &str, InputKind, &str, bool)] = &[
    ("name", "Name", InputKind::Text, "160", true),
    (
        "registration_number",
        "Registration number",
        InputKind::Text,
        "255",
        false,
    ),
    ("tax_id", "Tax ID", InputKind::Text, "64", false),
    ("email", "Email", InputKind::Email, "254", false),
    ("phone", "Phone", InputKind::Tel, "64", false),
];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Existing,
    New,
}
pub enum PartySubmission {
    Done(Value),
    Invalid(PartyErrors),
    Refused(String),
    Review,
}
pub enum Relation {
    Recorded { since: Option<String> },
    Missing,
    Hidden,
}
pub struct PartyChooser {
    prefix: String,
    form: &'static str,
    mode: Mode,
    kind: String,
    query: String,
    hits: Vec<Hit>,
    existing: HashMap<String, String>,
    existing_label: &'static str,
    existing_link: &'static str,
    selected: Option<Resolved>,
    code: String,
    hit_codes: HashMap<String, String>,
    new: HashMap<String, String>,
    candidates: Vec<Value>,
    likely: bool,
    reason: String,
}
impl PartyChooser {
    pub fn new(prefix: &str, form: &'static str) -> Self {
        Self {
            prefix: prefix.into(),
            form,
            mode: Mode::Existing,
            kind: "company".into(),
            query: String::new(),
            hits: Vec::new(),
            existing: HashMap::new(),
            existing_label: "",
            existing_link: "",
            selected: None,
            code: String::new(),
            hit_codes: HashMap::new(),
            new: HashMap::new(),
            candidates: Vec::new(),
            likely: false,
            reason: String::new(),
        }
    }
    pub fn existing(
        mut self,
        label: &'static str,
        link: &'static str,
        records: HashMap<String, String>,
    ) -> Self {
        self.existing_label = label;
        self.existing_link = link;
        self.existing = records;
        self
    }
    pub fn labels(&self) -> Vec<(String, String, String)> {
        let mut labels = vec![(
            "party".into(),
            format!("{}-company-search", self.prefix),
            "Company or person".into(),
        )];
        for (key, label, ..) in NEW_FIELDS {
            labels.push((
                format!("company_{key}"),
                format!("{}-company-{key}", self.prefix),
                (*label).into(),
            ));
        }
        labels.push((
            "company_reason".into(),
            format!("{}-company-reason", self.prefix),
            "Reason".into(),
        ));
        labels
    }
    pub fn read(&mut self, form: &HashMap<String, String>) {
        if let Some(kind) = form
            .get("party_kind")
            .filter(|s| KINDS.contains(&s.as_str()))
        {
            if self.kind != *kind {
                self.kind = kind.clone();
                self.candidates.clear();
                self.likely = false;
                self.reason.clear();
                if kind == "person" {
                    self.new.remove("tax_id");
                    self.new.remove("registration_number");
                }
            }
        }
        for (key, ..) in NEW_FIELDS {
            if self.kind == "person" && matches!(*key, "tax_id" | "registration_number") {
                continue;
            }
            if let Some(value) = form.get(&format!("company_{key}")) {
                self.new.insert((*key).into(), value.trim().into());
            }
        }
        if let Some(reason) = form.get("company_reason") {
            self.reason = reason.trim().into();
        }
    }
    async fn search(&mut self, pages: &PartyPickerContext, scope: &Scope) -> Result<()> {
        self.hits.clear();
        self.hit_codes.clear();
        if self.query.trim().is_empty() {
            return Ok(());
        }
        for (kind, contract) in [
            ("company", "core.company.list"),
            ("person", "core.person.list"),
        ] {
            let result = match pages
                .read_core(scope, contract, json!({"search":self.query,"page_size":25}))
                .await
            {
                Ok(v) => v,
                Err(Error::Forbidden | Error::Unauthorized) => continue,
                Err(e) => return Err(e),
            };
            let values = result
                .get("entries")
                .or_else(|| result.get("records"))
                .unwrap_or(&result);
            let mut refs = Vec::new();
            for row in values.as_array().into_iter().flatten() {
                if let Some(id) = row["id"].as_i64() {
                    let key = format!("{kind}:{id}");
                    if let Some(code) = row["code"].as_str() {
                        self.hit_codes.insert(key, code.into());
                    }
                    refs.push(Reference::new(kind, id.to_string()));
                }
            }
            for resolved in pages.records.resolve(scope, &refs).await? {
                if let Some(label) = resolved.label {
                    self.hits.push(Hit {
                        kind: kind.into(),
                        id: resolved.id,
                        label,
                        path: resolved.path,
                    });
                }
            }
        }
        self.hits.sort_by(|a, b| {
            a.label
                .cmp(&b.label)
                .then(a.kind.cmp(&b.kind))
                .then(a.id.cmp(&b.id))
        });
        Ok(())
    }
    async fn choose(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        value: &str,
    ) -> Result<()> {
        let (kind, id) = value.split_once(':').ok_or(Error::Invalid)?;
        if !KINDS.contains(&kind) || self.existing.contains_key(value) {
            return Err(Error::Invalid);
        }
        let resolved = pages
            .records
            .resolve(scope, &[Reference::new(kind, id)])
            .await?
            .into_iter()
            .next()
            .filter(|s| s.label.is_some())
            .ok_or(Error::Invalid)?;
        let numeric = id.parse::<i64>().map_err(|_| Error::Invalid)?;
        let (contract, input) = if kind == "person" {
            ("core.person.get", json!({"person_id":numeric}))
        } else {
            ("core.company.get", json!({"company_id":numeric}))
        };
        let core = pages.read_core(scope, contract, input).await?;
        self.code = core["code"].as_str().unwrap_or("").into();
        self.kind = kind.into();
        self.selected = Some(resolved);
        self.mode = Mode::Existing;
        Ok(())
    }
    pub async fn submit_composed(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: Value,
    ) -> Result<PartySubmission> {
        input["company_id"] = json!(match PartyPickerContext::company(scope) {
            Some(id) => id,
            None => return Ok(PartySubmission::Refused(DENIED.into())),
        });
        let reviewing = !self.candidates.is_empty() || self.likely;
        if self.mode == Mode::New {
            let mut new = json!({});
            for (key, ..) in NEW_FIELDS {
                if self.kind == "person" && matches!(*key, "tax_id" | "registration_number") {
                    continue;
                }
                put(&mut new, key, self.new.get(*key).map_or("", String::as_str));
            }
            if self.kind == "person" {
                new["kind"] = json!("human");
                input["new_person"] = new;
            } else {
                if reviewing {
                    if self.reason.is_empty() {
                        return Ok(PartySubmission::Invalid(errors_of(
                            "company_reason",
                            "give a reason to create another company",
                        )));
                    }
                    new["create_anyway"] = json!({"candidate_ids":self.candidates.iter().filter_map(|c|c["id"].as_i64()).collect::<Vec<_>>(),"reason":self.reason});
                }
                input["new_company"] = new;
            }
        } else {
            let Some(selected) = &self.selected else {
                return Ok(PartySubmission::Invalid(errors_of(
                    "party",
                    "choose a company or person",
                )));
            };
            input["record_kind"] = json!(selected.reference.kind);
            input["record_id"] = json!(selected.id.parse::<i64>().map_err(|_| Error::Invalid)?);
        }
        let review = match pages
            .review_core(scope, credential, event, contract, input.clone())
            .await?
        {
            Reviewed::Valid { review } => review,
            Reviewed::Invalid(fields) => {
                return Ok(PartySubmission::Invalid(composed_errors(fields)));
            }
            Reviewed::Refused(message) => return Ok(PartySubmission::Refused(message)),
        };
        let creation = &review["changes"]["target"]["creation"];
        let candidates = creation["duplicate_candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let likely = creation["duplicates_likely"].as_bool().unwrap_or(false);
        let changed = candidates
            .iter()
            .map(|c| c["id"].clone())
            .collect::<Vec<_>>()
            != self
                .candidates
                .iter()
                .map(|c| c["id"].clone())
                .collect::<Vec<_>>();
        if self.kind == "company" && (!reviewing || changed) && (!candidates.is_empty() || likely) {
            self.candidates = candidates;
            self.likely = likely;
            return Ok(PartySubmission::Review);
        }
        Ok(
            match pages
                .commit_core(scope, credential, event, contract, input)
                .await?
            {
                Written::Done(record) => PartySubmission::Done(record),
                Written::Invalid(fields) => PartySubmission::Invalid(composed_errors(fields)),
                Written::Refused(message) => PartySubmission::Refused(message),
            },
        )
    }
    fn render_new(&self, errors: &PartyErrors) -> Result<Html> {
        let mut body = render(
            Field::in_form(self.form, "party_kind")
                .id(format!("{}-party-kind", self.prefix))
                .label("New record")
                .kind(InputKind::Select)
                .value(&self.kind)
                .options(bilibi_ui::choices(&[
                    ("Company", "company"),
                    ("Person", "person"),
                ])),
        )?;
        for (key, label, kind, max, required) in NEW_FIELDS {
            if self.kind == "person" && matches!(*key, "tax_id" | "registration_number") {
                continue;
            }
            let mut field = Field::in_form(self.form, &format!("company_{key}"))
                .id(format!("{}-company-{key}", self.prefix))
                .label(*label)
                .kind(*kind)
                .value(self.new.get(*key).cloned().unwrap_or_default())
                .attr("maxlength", max)
                .errors(errors.under(&format!("company_{key}"), label));
            if *required {
                field = field.required();
            }
            body.push(&render(field)?);
        }
        for (i, candidate) in self.candidates.iter().enumerate() {
            body.push(&render(Button {
                id: &format!("{}-candidate-{i}", self.prefix),
                button_type: "button",
                event: "use_candidate",
                name: "company",
                value: &candidate["id"].to_string(),
                body: Html::text(&format!(
                    "Use {}",
                    candidate["name"].as_str().unwrap_or("existing company")
                )),
                ..Button::default()
            })?);
        }
        if self.likely || !self.candidates.is_empty() {
            body.push(&render(
                Field::in_form(self.form, "company_reason")
                    .id(format!("{}-company-reason", self.prefix))
                    .label("Reason to create another company")
                    .value(&self.reason)
                    .errors(errors.under("company_reason", "Reason")),
            )?);
        }
        body.push(&render(Button {
            id: &format!("{}-cancel-new", self.prefix),
            button_type: "button",
            event: "cancel_new_company",
            body: Html::text("Cancel new record"),
            ..Button::default()
        })?);
        Ok(body)
    }
}

pub struct PartyRow {
    pub id: String,
    pub name: String,
    pub code: String,
    pub errors: PartyErrors,
    resolved: Resolved,
}
pub struct MultiPartyChooser {
    chooser: PartyChooser,
    rows: Vec<PartyRow>,
}
impl MultiPartyChooser {
    pub fn new(prefix: &str, form: &'static str) -> Self {
        Self {
            chooser: PartyChooser::new(prefix, form),
            rows: Vec::new(),
        }
    }
    pub fn existing(
        mut self,
        label: &'static str,
        link: &'static str,
        records: HashMap<String, String>,
    ) -> Self {
        self.chooser = self.chooser.existing(label, link, records);
        self
    }
    pub fn labels(&self) -> Vec<(String, String, String)> {
        self.chooser.labels()
    }
    pub fn rows(&self) -> &[PartyRow] {
        &self.rows
    }
    pub fn is_new(&self) -> bool {
        self.chooser.mode == Mode::New
    }
    pub fn count(&self) -> usize {
        self.rows.len() + usize::from(self.is_new())
    }
    pub fn read(&mut self, form: &HashMap<String, String>) {
        self.chooser.read(form);
    }
    pub async fn search(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        query: &str,
    ) -> Result<()> {
        self.chooser.query = query.chars().take(255).collect();
        self.chooser.search(pages, scope).await
    }
    pub async fn pick(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        if self.rows.iter().any(|r| r.id == id) {
            self.remove(id);
            return Ok(());
        }
        if !self
            .chooser
            .hits
            .iter()
            .any(|h| format!("{}:{}", h.kind, h.id) == id)
        {
            return Err(Error::Invalid);
        }
        let was_new = self.is_new();
        self.chooser.choose(pages, scope, id).await?;
        self.capture()?;
        if was_new {
            self.chooser.mode = Mode::New;
        }
        Ok(())
    }
    /// Adds a party that arrived by reference (a drop or an "Add to…" link, see
    /// `bilibi_records::Records::receive`) as a pick of the search would: it
    /// is read under the reader's own scope and refused when it is already
    /// listed, an existing customer or not available. No prior search is
    /// needed, since the reference names the record itself.
    pub async fn add(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        kind: &str,
        id: &str,
    ) -> Result<()> {
        let value = format!("{kind}:{id}");
        if self.rows.iter().any(|r| r.id == value) {
            return Ok(());
        }
        let was_new = self.is_new();
        self.chooser.choose(pages, scope, &value).await?;
        self.capture()?;
        if was_new {
            self.chooser.mode = Mode::New;
        }
        Ok(())
    }
    fn capture(&mut self) -> Result<()> {
        let resolved = self.chooser.selected.take().ok_or(Error::Invalid)?;
        let id = format!("{}:{}", resolved.reference.kind, resolved.id);
        if !self.rows.iter().any(|r| r.id == id) {
            self.rows.push(PartyRow {
                id,
                name: resolved.label.clone().ok_or(Error::Invalid)?,
                code: self.chooser.code.clone(),
                errors: PartyErrors::default(),
                resolved,
            });
        }
        Ok(())
    }
    pub fn mark_existing(&mut self, id: &str, href: String) {
        self.chooser.existing.insert(id.into(), href);
    }
    pub fn remove(&mut self, id: &str) {
        self.rows.retain(|r| r.id != id);
    }
    pub fn start_new(&mut self, query: &str) {
        self.chooser.mode = Mode::New;
        self.chooser.kind = "company".into();
        self.chooser.new.clear();
        self.chooser.new.insert("name".into(), query.trim().into());
        self.chooser.candidates.clear();
        self.chooser.likely = false;
        self.chooser.reason.clear();
    }
    pub fn cancel_new(&mut self) {
        self.chooser.mode = Mode::Existing;
    }
    pub fn resume_new(&mut self) {
        self.chooser.mode = Mode::New;
    }
    pub async fn use_candidate(
        &mut self,
        pages: &PartyPickerContext,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let numeric = id.parse::<i64>().map_err(|_| Error::Invalid)?;
        if !self
            .chooser
            .candidates
            .iter()
            .any(|c| c["id"].as_i64() == Some(numeric))
        {
            return Err(Error::Invalid);
        }
        self.chooser
            .choose(pages, scope, &format!("company:{numeric}"))
            .await?;
        self.chooser.candidates.clear();
        self.chooser.likely = false;
        self.capture()
    }
    pub fn activate(&mut self, id: &str) -> Result<()> {
        let row = self
            .rows
            .iter()
            .find(|r| r.id == id)
            .ok_or(Error::Invalid)?;
        self.chooser.selected = Some(row.resolved.clone());
        self.chooser.kind = row.resolved.reference.kind.clone();
        self.chooser.mode = Mode::Existing;
        Ok(())
    }
    pub fn row_errors(&mut self, id: &str, errors: PartyErrors) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.errors = errors;
        }
    }
    pub fn chooser(&mut self) -> &mut PartyChooser {
        &mut self.chooser
    }
    pub fn render(&self, errors: &PartyErrors) -> Result<Html> {
        let selected = self
            .rows
            .iter()
            .map(|r| SearchMultiOption::new(&r.id, &r.name).detail(&r.code))
            .collect();
        let results = self
            .chooser
            .hits
            .iter()
            .map(|h| {
                let id = format!("{}:{}", h.kind, h.id);
                let option = SearchMultiOption::new(&id, &h.label).detail(format!(
                    "{} · {}",
                    if h.kind == "person" {
                        "Person"
                    } else {
                        "Company"
                    },
                    self.chooser
                        .hit_codes
                        .get(&id)
                        .map(String::as_str)
                        .unwrap_or("")
                ));
                match self.chooser.existing.get(&id) {
                    Some(href) => option.disabled(self.chooser.existing_label, Some(href.clone())),
                    None => option,
                }
            })
            .collect();
        let mut body = render(
            SearchMultiSelect::new(
                format!("{}-company-search", self.chooser.prefix),
                format!("{}[parties]", self.chooser.form),
                "Companies and people",
                "search_companies",
                "pick_companies",
                "remove_company",
            )
            .query(&self.chooser.query)
            .selected(selected)
            .results(results)
            .create("Create company or person", "new_company")
            .wrap_chips(),
        )?;
        for row in &self.rows {
            if !row.errors.is_empty() {
                body.push(&render(bilibi_ui::Alert {
                    id: &format!(
                        "{}-{}-errors",
                        self.chooser.prefix,
                        row.id.replace(':', "-")
                    ),
                    kind: StatusKind::Error,
                    body: Html::text(&format!("{}: {}", row.name, sentences(&row.errors.0))),
                    ..bilibi_ui::Alert::default()
                })?);
            }
        }
        if self.is_new() {
            body.push(&self.chooser.render_new(errors)?);
        }
        Ok(body)
    }
}

/// Whether the current company records `target` as its customer now.
pub async fn relation(
    pages: &PartyPickerContext,
    scope: &Scope,
    target_kind: &str,
    target: i64,
) -> Result<Relation> {
    let Some(company) = PartyPickerContext::company(scope) else {
        return Ok(Relation::Hidden);
    };
    let links = match pages
        .read_core(
            scope,
            "core.company.relationship.list",
            json!({"company_id": company}),
        )
        .await
    {
        Ok(links) => links,
        Err(Error::Forbidden | Error::Unauthorized) => return Ok(Relation::Hidden),
        Err(error) => return Err(error),
    };
    Ok(links
        .as_array()
        .into_iter()
        .flatten()
        .find(|link| {
            link["direction"] == "outgoing"
                && link["role_code"] == pages.role
                && link["current"] == true
                && link["other"]["kind"]
                    == if target_kind == "company" {
                        "organisation"
                    } else {
                        "person"
                    }
                && link["other"]["id"].as_i64() == Some(target)
        })
        .map_or(Relation::Missing, |link| Relation::Recorded {
            since: link["dates"]["effective_from"].as_str().map(str::to_owned),
        }))
}

/// Records `target` as a customer of the current company through Core's
/// reviewed write, unless Core already records it. Answers why it could not.
pub async fn record_relationship(
    pages: &PartyPickerContext,
    scope: &Scope,
    credential: &str,
    event: &str,
    target_kind: &str,
    target: i64,
) -> Result<std::result::Result<(), String>> {
    let Some(company) = PartyPickerContext::company(scope) else {
        return Ok(Err(DENIED.into()));
    };
    if let Relation::Recorded { .. } = relation(pages, scope, target_kind, target).await? {
        return Ok(Ok(()));
    }
    let roles = match pages
        .read_core(scope, "core.company.relationship_role.list", json!({}))
        .await
    {
        Ok(roles) => roles,
        Err(Error::Forbidden | Error::Unauthorized) => {
            return Ok(Err(format!(
                "you need permission to list companies to find the {} role",
                pages.role
            )));
        }
        Err(error) => return Err(error),
    };
    let Some(role) = roles
        .as_array()
        .into_iter()
        .flatten()
        .find(|role| role["code"] == pages.role)
        .and_then(|role| role["id"].as_i64())
    else {
        return Ok(Err(format!(
            "the relationship roles have no {} role; add one under Companies › Relationship Roles",
            pages.role
        )));
    };
    match pages
        .write_core(
            scope,
            credential,
            event,
            "core.company.relationship.assign",
            json!({
                "source_organisation_id": company,
                "target_kind": if target_kind == "company" {"organisation"} else {"person"},
                "target_id": target,
                "role_id": role,
            }),
        )
        .await?
    {
        Written::Done(_) => Ok(Ok(())),
        Written::Invalid(fields) => Ok(Err(sentences(&fields))),
        Written::Refused(message) => Ok(Err(worded(
            message,
            "you do not have permission to change the current company's relationships",
        ))),
    }
}

/// The flash after a customer was added or linked, saying whether Core also
/// records the relationship.
pub fn relationship_flash(
    role: &str,
    done: &str,
    recorded: &std::result::Result<(), String>,
) -> (StatusKind, String) {
    match recorded {
        Ok(()) => (
            StatusKind::Success,
            format!("{done} The {role} relationship is recorded in Companies."),
        ),
        Err(reason) => (
            StatusKind::Warning,
            format!(
                "{done} The {role} relationship was not recorded: {reason}. Record it on the company's Relationships page."
            ),
        ),
    }
}

fn errors_of(path: &str, message: &str) -> PartyErrors {
    PartyErrors(BTreeMap::from([(
        path.to_owned(),
        vec![message.to_owned()],
    )]))
}

/// Core's field names on the dialog's company fields. The dialog has no code
/// field: Core derives the code from the name, so a taken code means a
/// company of that name already exists and should be chosen instead.
fn composed_errors(fields: BTreeMap<String, Vec<String>>) -> PartyErrors {
    PartyErrors(
        fields
            .into_iter()
            .map(|(field, messages)| {
                let key = if matches!(
                    field.as_str(),
                    "record_id" | "record_kind" | "new_company" | "new_person"
                ) {
                    "party".to_owned()
                } else if let Some(rest) = field
                    .strip_prefix("new_company.")
                    .or_else(|| field.strip_prefix("new_person."))
                {
                    if rest == "create_anyway" {
                        "company_reason".into()
                    } else if rest == "code" {
                        "company_name".into()
                    } else {
                        format!("company_{rest}")
                    }
                } else {
                    field
                };
                (key, messages)
            })
            .collect(),
    )
}

/// Shared Core party picker. Each page supplies its own declared service
/// and relationship role; every Core write uses that page's live event.
#[derive(Clone)]
pub struct PartyPickerContext {
    pub records: Arc<bilibi_records::Records>,
    pub contracts: Arc<PageContracts>,
    pub service: &'static str,
    pub role: &'static str,
}
impl PartyPickerContext {
    fn company(scope: &Scope) -> Option<i64> {
        scope.current_company().map(|id| id.get())
    }
    async fn read_core(&self, scope: &Scope, key: &str, input: Value) -> Result<Value> {
        let mut answer = self
            .contracts
            .read(scope, key, &input)
            .await
            .map_err(read_error)?;
        Ok(answer["result"].take())
    }
    pub fn call<'a>(
        &self,
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            service: self.service,
            event,
            contract,
        }
    }

    /// A reviewed write of another module's contract on the person's behalf,
    /// with the input as given: Core names its own company, so none is added.
    pub async fn write_core(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Written> {
        let answer = self
            .contracts
            .submit(&self.call(scope, credential, event, contract), input)
            .await;
        Self::written(contract, event, answer)
    }

    /// The review half of a confirmation on another module's contract.
    pub async fn review_core(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Reviewed> {
        match self
            .contracts
            .preview(&self.call(scope, credential, event, contract), input)
            .await
        {
            Ok(value) => {
                let result = &value["results"][0];
                if result["valid"].as_bool() == Some(true) {
                    return Ok(Reviewed::Valid {
                        review: result["preview"]["result"].clone(),
                    });
                }
                let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
                if let Some(map) = result["error"]["fields"].as_object() {
                    for (field, messages) in map {
                        fields.insert(
                            field.clone(),
                            messages
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|m| m.as_str().map(str::to_owned))
                                .collect(),
                        );
                    }
                }
                if fields.is_empty() {
                    return Ok(Reviewed::Refused(
                        if result["error"]["code"].as_str() == Some("missing_capability") {
                            DENIED.into()
                        } else {
                            "That could not be reviewed. Please try again.".into()
                        },
                    ));
                }
                Ok(Reviewed::Invalid(fields))
            }
            Err(error) => Ok(match Self::written(contract, event, Err(error))? {
                Written::Invalid(fields) => Reviewed::Invalid(fields),
                Written::Refused(message) => Reviewed::Refused(message),
                Written::Done(_) => Reviewed::Refused(DENIED.into()),
            }),
        }
    }

    /// Commits a reviewed confirmation with the same input. A
    /// commit answers the plan's envelope (`results[0].result`), where a
    /// submit answers the one result itself.
    pub async fn commit_core(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Written> {
        let answer = self
            .contracts
            .commit(&self.call(scope, credential, event, contract), input)
            .await
            .map(|mut value| value["results"][0]["result"].take());
        Self::written(contract, event, answer)
    }

    fn written(
        contract: &str,
        event: &str,
        answer: std::result::Result<Value, ContractError>,
    ) -> Result<Written> {
        match answer {
            Ok(mut answer) => Ok(Written::Done(answer["result"]["record"].take())),
            Err(ContractError::InvalidInput(fields)) => Ok(Written::Invalid(fields)),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                Ok(Written::Refused(DENIED.into()))
            }
            Err(ContractError::Backend(Error::Conflict)) => Ok(Written::Refused(
                "The details changed. Review them again before saving.".into(),
            )),
            Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => Ok(
                Written::Refused("That record no longer exists. Reload the page.".into()),
            ),
            // The host signs the browser in again, or sends it to confirm.
            Err(ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => Err(error),
            Err(error) => {
                eprintln!("receivables page: {contract} from {event} failed: {error:?}");
                Ok(Written::Refused(
                    "That could not be saved. Please try again.".into(),
                ))
            }
        }
    }
}
/// A refusal for a missing permission, worded for what the person was doing.
pub fn worded(message: String, denied: &str) -> String {
    if message == DENIED {
        denied.into()
    } else {
        message
    }
}

pub const DENIED: &str = "You do not have permission to do that.";

/// What a write came to.
pub enum Written {
    Done(Value),
    /// Field errors keyed by the input's path (`lines.0.quantity`, `base`).
    Invalid(BTreeMap<String, Vec<String>>),
    Refused(String),
}

/// What a review came to: the owner's review or the refusal.
pub enum Reviewed {
    Valid { review: Value },
    Invalid(BTreeMap<String, Vec<String>>),
    Refused(String),
}

fn read_error(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        ContractError::Forbidden => Error::Forbidden,
        ContractError::NotFound => Error::NotFound,
        ContractError::InvalidInput(_) => Error::Invalid,
        _ => Error::Internal,
    }
}

#[derive(Clone, Debug, Default)]
pub struct PartyErrors(pub BTreeMap<String, Vec<String>>);

impl PartyErrors {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The messages for one input field.
    pub fn of(&self, path: &str) -> Vec<String> {
        self.0.get(path).cloned().unwrap_or_default()
    }

    /// The messages for one input field as sentences that name it, the way
    /// the summary reads them, for the line under the field.
    pub fn under(&self, path: &str, label: &str) -> Vec<String> {
        self.of(path)
            .iter()
            .map(|message| error_sentence(label, message))
            .collect()
    }

    /// The messages that belong to no field (a refusal of the whole request).
    pub fn base(&self) -> Vec<String> {
        self.of("base")
    }

    /// Everything the form was refused for, each sentence linked to the field
    /// that holds it. `labels` names `(path, dom id, label)` for the fields on
    /// the form; any other path is shown by its own words.
    pub fn summary(&self, id: &str, labels: &[(String, String, String)]) -> ErrorSummary {
        let mut summary = ErrorSummary::new(id);
        for (path, messages) in &self.0 {
            let known = labels.iter().find(|(p, _, _)| p == path);
            for message in messages {
                match known {
                    Some((_, dom, label)) => summary.push(dom, error_sentence(label, message)),
                    None => summary.push("", message.clone()),
                }
            }
        }
        summary
    }
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn put(input: &mut Value, key: &str, text: &str) {
    if !text.is_empty() {
        input[key] = json!(text);
    }
}
fn sentences(fields: &BTreeMap<String, Vec<String>>) -> String {
    fields
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |m| error_sentence(k, m)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The company's billing address as of `today`, from Core Address's owner
/// list: the attachment carrying the billing kind within its dates, primary
/// first (the list's order), else the primary address, else the first.
pub fn party_billing_lines(rows: &Value, today: &str) -> Vec<String> {
    let attachment = |row: &Value| row["attachment"]["attachment"].clone();
    let current: Vec<&Value> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            let dates = &row["attachment"]["attachment"]["dates"];
            let from = dates["effective_from"].as_str();
            let to = dates["effective_to"].as_str();
            today.is_empty() || (from.is_none_or(|f| f <= today) && to.is_none_or(|t| t >= today))
        })
        .collect();
    let billing = |row: &Value| {
        attachment(row)["kinds"]
            .as_array()
            .is_some_and(|k| k.iter().any(|v| v == "billing"))
    };
    let primary = |row: &Value| attachment(row)["is_primary"] == true;
    let chosen = current
        .iter()
        .find(|row| billing(row))
        .or_else(|| current.iter().find(|row| primary(row)))
        .or_else(|| current.first());
    let Some(row) = chosen else {
        return Vec::new();
    };
    let a = &row["address"]["address"];
    let text = |key: &str| a[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let mut lines: Vec<String> = ["line1", "line2", "line3"]
        .into_iter()
        .filter_map(text)
        .map(str::to_owned)
        .collect();
    let place: Vec<&str> = ["postcode", "locality", "admin1_code", "country_iso"]
        .into_iter()
        .filter_map(text)
        .collect();
    if !place.is_empty() {
        lines.push(place.join(" "));
    }
    lines
}
