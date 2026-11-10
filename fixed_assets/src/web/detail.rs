//! Bilimbi Finance.FixedAssets.Web.AssetLive, including immutable history.
//! The header offers the one next step the asset's state allows (sell or
//! write off, reverse the disposal, or void a reversed acquisition); each
//! opens its dialog, also from the register row's icon button (`?act=`).
mod lifecycle;
use super::{categories, input::Draft, *};
use crate::{AssetDetail, EventAction, DISPOSE, REVERSE, VOID};
use bilibi_ui::{BackLink, Card, ConfirmDialog, Fact, Facts, FlashMap, InputKind, SectionHeading};
use lifecycle::{event_name, Act, Standing};

pub(super) struct Detail {
    pages: Pages,
    asset_id: i64,
    schedule: ListColumns,
    charges: ListColumns,
    events: ListColumns,
    evidence: super::source_evidence::EvidenceView,
    /// The open dialog and its draft.
    dialog: Option<(Act, Draft)>,
    pending_void: Option<(Draft, serde_json::Value)>,
    flash: Option<(StatusKind, String)>,
}
#[derive(Template)]
#[template(path = "detail.html")]
struct View<'a> {
    title: String,
    subtitle: String,
    actions: Html,
    record: Option<i64>,
    unavailable: Html,
    flash: FlashMap<'a>,
    sections: Vec<Html>,
    dialogs: Html,
}

impl Detail {
    pub(super) fn new(
        pages: &Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
        _framed: bool,
    ) -> Result<Self> {
        let asset_id = params::positive_integer(path.get("id").map(String::as_str))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or(Error::NotFound)?;
        Ok(Self {
            pages: pages.clone(),
            evidence: Default::default(),
            asset_id,
            schedule: cols(
                "asset-schedule",
                &[
                    ("month", "Month"),
                    ("amount", "Depreciation"),
                    ("accumulated", "Accumulated"),
                    ("carrying", "Carrying value"),
                ],
            )?,
            charges: cols(
                "asset-charges",
                &[
                    ("month", "Month"),
                    ("amount", "Depreciation"),
                    ("entry", "Journal entry"),
                    ("actor", "Posted by"),
                    ("recorded", "Posted"),
                    ("reversal", "Reversed"),
                ],
            )?,
            events: cols(
                "asset-events",
                &[
                    ("action", "Change"),
                    ("effective", "Date"),
                    ("entry", "Journal entry"),
                    ("reason", "Reason"),
                    ("actor", "By"),
                    ("recorded", "Recorded"),
                ],
            )?,
            dialog: query
                .get("act")
                .and_then(|a| Act::parse(a))
                .map(|a| (a, Draft::default())),
            pending_void: None,
            flash: None,
        })
    }

    async fn body(&mut self, s: &Scope) -> Result<String> {
        let (units, accounts) = self.pages.account_context(s).await?;
        let display = self.pages.display(s).await?;
        let read = match company(s) {
            Some(c) => {
                self.pages
                    .read::<AssetDetail>(
                        s,
                        "finance.fixed_assets.asset.get",
                        serde_json::json!({"company_id":c,"asset_id":self.asset_id}),
                        "record",
                    )
                    .await
            }
            None => Err(FixedAssetsError::CompanyNotFound),
        };
        let back = render(BackLink {
            href: "/finance/fixed-assets/register",
            id: "asset-back",
            title: "Back to fixed assets",
            ..BackLink::default()
        })?;
        let d = match read {
            Ok(d) => d,
            Err(e) => {
                return View {
                    title: "Fixed asset".into(),
                    subtitle: String::new(),
                    actions: back,
                    record: None,
                    unavailable: render(
                        EmptyState::missing("Asset unavailable", refusal(e))
                            .id("asset-unavailable"),
                    )?,
                    flash: flash_map(&self.flash),
                    sections: vec![],
                    dialogs: Html::EMPTY,
                }
                .render()
                .map_err(|_| Error::Internal);
            }
        };
        let standing = Standing::of(&d, self.source_reversed(s, &d).await);
        let next = standing.next();
        // An action the state does not allow, or this person may not take, is
        // not offered: a dialog the address asked for closes.
        let allowed = match next {
            Some(act) => self.pages.allowed(s, act.cap()).await?,
            None => false,
        };
        if self
            .dialog
            .as_ref()
            .is_some_and(|(act, _)| Some(*act) != next || !allowed)
        {
            self.dialog = None;
        }
        let mut actions = back;
        if let (Some(act), true) = (next, allowed) {
            actions.push(&header_button(
                &format!("asset-{}", act.key()),
                &format!("open_{}", act.key()),
                act.verb(),
                true,
            )?);
        }
        let people = self
            .pages
            .people(
                s,
                std::iter::once(d.asset.recorded_by_user_id)
                    .chain(d.asset.recorded_by_impersonator_id)
                    .chain(d.charges.iter().flat_map(|c| {
                        std::iter::once(c.recorded_by_user_id).chain(c.recorded_by_impersonator_id)
                    }))
                    .chain(d.events.iter().flat_map(|e| {
                        std::iter::once(e.recorded_by_user_id).chain(e.recorded_by_impersonator_id)
                    })),
            )
            .await?;
        let mut sections = vec![self.facts(s, &d, &standing, units, &people).await?];
        if standing.voided {
            sections.push(render(bilibi_ui::Alert {
                id: "asset-voided",
                kind: StatusKind::Info,
                body: Html::text(
                    "Acquisition voided: this asset is left out of depreciation runs and the ledger check.",
                ),
                ..Default::default()
            })?);
        }
        if let Some(opening) = &d.opening {
            let fact = |title, value: String| Fact {
                title,
                value: Html::text(&value),
                ..Default::default()
            };
            sections.push(render(Card {
                id: "asset-opening",
                title: "Opening register",
                body: render(Facts {
                    id: "asset-opening-facts",
                    items: vec![
                        fact(
                            "Source reference",
                            opening["source_reference"]
                                .as_str()
                                .unwrap_or("")
                                .to_owned(),
                        ),
                        fact(
                            "Source asset",
                            opening["source_id"].as_str().unwrap_or("").to_owned(),
                        ),
                        fact(
                            "Conversion date",
                            bilibi_finance_ledger::dates::display_text(
                                opening["opening_date"].as_str().unwrap_or(""),
                            ),
                        ),
                        fact(
                            "Source accumulated depreciation",
                            d.asset.opening_accumulated.display(units),
                        ),
                        fact(
                            "Source carrying value",
                            (d.asset.cost - d.asset.opening_accumulated).display(units),
                        ),
                        fact(
                            "Prospective depreciation",
                            crate::opening::CONVENTION.into(),
                        ),
                    ],
                })?,
                ..Default::default()
            })?);
            sections.push(self.evidence.render(
                opening["source_id"].as_str().unwrap_or(""),
                &opening["source_evidence"],
                units.ok_or(Error::Internal)?,
            )?);
        }
        let rows = d
            .schedule
            .iter()
            .map(|r| {
                FlexRow::new(r.period.to_string())
                    .cell("month", Cell::text(month(r.period)))
                    .cell("amount", Cell::text(r.amount.display(units)))
                    .cell("accumulated", Cell::text(r.accumulated.display(units)))
                    .cell("carrying", Cell::text(r.carrying_value.display(units)))
            })
            .collect();
        let mut schedule = render(SectionHeading {
            title: "Projected depreciation",
            id: "asset-schedule-heading",
            description: "Assumes the asset stays in use. Depreciation runs post it.",
            ..Default::default()
        })?;
        schedule.push(&table(
            "asset-schedule",
            &self.schedule,
            rows,
            "Projected depreciation",
            "schedule_grid",
            "",
        )?);
        sections.push(schedule);
        let mut rows = Vec::new();
        for r in &d.charges {
            let reversal = match &r.reversal {
                Some(e) => {
                    let mut html = Html::text(&format!(
                        "{} ",
                        bilibi_finance_ledger::dates::display(e.effective_date)
                    ));
                    if e.entry_id.is_some() {
                        html.push(&entry_link(e.entry_id, e.entry_number)?);
                    }
                    html.push(&Html::text(&format!(": {}", e.reason)));
                    html
                }
                None => Html::text("—"),
            };
            rows.push(
                FlexRow::new(r.run_id.to_string())
                    .cell("month", Cell::text(month(r.period)))
                    .cell("amount", Cell::text(r.amount.display(units)))
                    .slot("entry", entry_link(r.entry_id, r.entry_number)?)
                    .cell(
                        "actor",
                        Cell::text(
                            people.name(r.recorded_by_user_id, r.recorded_by_impersonator_id),
                        ),
                    )
                    .cell("recorded", time_cell(&display, &r.recorded_at)?)
                    .slot("reversal", reversal),
            );
        }
        let mut charges = render(SectionHeading {
            title: "Posted depreciation",
            id: "asset-charges-heading",
            ..Default::default()
        })?;
        charges.push(&table(
            "asset-charges",
            &self.charges,
            rows,
            "Posted depreciation",
            "charges_grid",
            "No depreciation posted yet.",
        )?);
        sections.push(charges);
        if !d.events.is_empty() {
            let mut rows = Vec::new();
            for e in &d.events {
                rows.push(
                    FlexRow::new(e.id.to_string())
                        .cell("action", Cell::text(event_name(e.action)))
                        .cell(
                            "effective",
                            Cell::text(bilibi_finance_ledger::dates::display(e.effective_date)),
                        )
                        .slot("entry", entry_link(e.entry_id, e.entry_number)?)
                        .cell("reason", Cell::text(e.reason.clone()))
                        .cell(
                            "actor",
                            Cell::text(
                                people.name(e.recorded_by_user_id, e.recorded_by_impersonator_id),
                            ),
                        )
                        .cell("recorded", time_cell(&display, &e.recorded_at)?),
                );
            }
            let mut events = render(SectionHeading {
                title: "History",
                id: "asset-events-heading",
                ..Default::default()
            })?;
            events.push(&table(
                "asset-events",
                &self.events,
                rows,
                "History",
                "events_grid",
                "",
            )?);
            sections.push(events);
        }
        let title = d.asset.description.clone();
        let mut dialogs = self.dialog(&accounts, &d.asset.description)?;
        if self.pending_void.is_some() {
            dialogs.push(&render(ConfirmDialog {
                id: "asset-void-confirm",
                consequence: "Void this acquisition?",
                detail: "Depreciation already posted for this asset is reversed; other assets keep theirs. This cannot be undone.",
                confirm: "Void acquisition",
                working: "Voiding…",
                confirm_event: "confirm_void",
                cancel_event: "cancel_void",
            })?);
        }
        View {
            title,
            subtitle: format!("Fixed asset {} · {}", d.asset.asset_number, d.revision.name),
            actions,
            record: Some(d.asset.id),
            unavailable: Html::EMPTY,
            flash: flash_map(&self.flash),
            sections,
            dialogs,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn input(&self, s: &Scope, d: &Draft) -> crate::Result<serde_json::Value> {
        let mut input = d.event_values();
        input["company_id"] = company(s).ok_or(FixedAssetsError::CompanyNotFound)?.into();
        input["asset_id"] = self.asset_id.into();
        Ok(input)
    }

    async fn perform(&self, s: &Scope, credential: &str, act: Act, d: &Draft) -> crate::Result<()> {
        let mut input = self.input(s, d)?;
        let (event, key) = if act == Act::Dispose {
            input["proceeds"] = serde_json::json!(d.value("proceeds"));
            let account = d.value("settlement_account_id").trim();
            if !account.is_empty() {
                input["settlement_account_id"] = account
                    .parse::<i64>()
                    .map_or_else(|_| serde_json::json!(account), |id| serde_json::json!(id));
            }
            ("dispose", "finance.fixed_assets.asset.dispose")
        } else {
            ("reverse_disposal", "finance.fixed_assets.disposal.reverse")
        };
        self.pages.submit(s, credential, event, key, input).await?;
        Ok(())
    }

    /// A refusal on the dialog's fields, or in its alert when it has none.
    fn refuse(&mut self, mut draft: Draft, e: FixedAssetsError, act: Act) {
        draft.refused(&e);
        if draft.errors.is_empty() {
            draft
                .errors
                .push(bilibi_finance_ledger::FieldError::new("base", refusal(e)));
        }
        self.dialog = Some((act, draft));
    }

    async fn event(&mut self, s: &Scope, credential: &str, v: &serde_json::Value) -> Result<()> {
        let event = v["type"].as_str().unwrap_or("");
        match event {
            event if event.starts_with("source_evidence_grid:") => {
                self.evidence.handle(event, &params::string_values(v));
                return Ok(());
            }
            "clear-flash" => {
                clear_flash(&mut self.flash, v);
                return Ok(());
            }
            "schedule_grid" => {
                self.schedule.handle(&params::string_values(v));
                return Ok(());
            }
            "charges_grid" => {
                self.charges.handle(&params::string_values(v));
                return Ok(());
            }
            "events_grid" => {
                self.events.handle(&params::string_values(v));
                return Ok(());
            }
            "close_dialog" => {
                self.dialog = None;
                return Ok(());
            }
            "cancel_void" => {
                if let Some((draft, _)) = self.pending_void.take() {
                    self.dialog = Some((Act::Void, draft));
                }
                return Ok(());
            }
            "open_dispose" | "open_reverse" | "open_void" => {
                let act = Act::parse(&event[5..]).ok_or(Error::Invalid)?;
                self.dialog = Some((act, Draft::default()));
                return Ok(());
            }
            "dispose" | "reverse_disposal" | "void" | "confirm_void" => {}
            _ => return Err(Error::Invalid),
        }
        let act = match event {
            "dispose" => Act::Dispose,
            "reverse_disposal" => Act::Reverse,
            _ => Act::Void,
        };
        if !self.pages.allowed(s, act.cap()).await? {
            self.pending_void = None;
            self.dialog = None;
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(());
        }
        if event == "confirm_void" {
            let Some((draft, input)) = self.pending_void.take() else {
                return Ok(());
            };
            match self
                .pages
                .contracts
                .commit(
                    &self.pages.call(
                        s,
                        credential,
                        event,
                        "finance.fixed_assets.acquisition.void",
                    ),
                    input,
                )
                .await
                .map_err(contract_error)
            {
                Ok(_) => {
                    self.dialog = None;
                    self.flash = Some((StatusKind::Success, "Acquisition voided.".into()));
                }
                Err(e) => self.refuse(draft, e, Act::Void),
            }
            return Ok(());
        }
        let draft = Draft::posted(v);
        if act == Act::Void {
            let review=async{
                let input=self.input(s,&draft)?;
                let result=self.pages.contracts.preview(&self.pages.call(s,credential,"void","finance.fixed_assets.acquisition.void"),input.clone()).await.map_err(contract_error)?;
                if result["results"][0]["valid"]!=true{
                    let fields=result["results"][0]["error"]["fields"].clone();
                    return Err(contract_error(bilibi_agent_api::Error::InvalidInput(serde_json::from_value(fields).unwrap_or_else(|_|std::collections::BTreeMap::from([("base".into(),vec!["The acquisition could not be reviewed. Check its source and permissions.".into()])])))));
                }
                Ok(input)
            }.await;
            match review {
                Ok(input) => {
                    self.dialog = None;
                    self.pending_void = Some((draft, input));
                }
                Err(e) => self.refuse(draft, e, Act::Void),
            }
            return Ok(());
        }
        match self.perform(s, credential, act, &draft).await {
            Ok(()) => {
                self.dialog = None;
                self.flash = Some((
                    StatusKind::Success,
                    match act {
                        Act::Dispose => "Disposal posted.",
                        _ => "Disposal reversed.",
                    }
                    .into(),
                ));
            }
            Err(e) => self.refuse(draft, e, act),
        }
        Ok(())
    }
}
#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::text(id="asset-recorded-at", cell=cell) %}{% endcall %}"#,
    ext = "html"
)]
struct Recorded {
    cell: Cell,
}
impl LiveState<Scope, Error> for Detail {
    fn update<'a>(&'a mut self, r: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match r.input {
                LiveInput::Event(v) => self.event(r.scope, r.credential, v).await?,
                LiveInput::Params(p) => {
                    if let Some(id) = p.get("id") {
                        let id = params::positive_integer(Some(id))
                            .and_then(|v| i64::try_from(v).ok())
                            .ok_or(Error::NotFound)?;
                        if id != self.asset_id {
                            self.asset_id = id;
                            self.pending_void = None;
                            self.dialog = None;
                        }
                    }
                    if let Some(act) = p.get("act").and_then(|a| Act::parse(a)) {
                        self.dialog = Some((act, Draft::default()));
                    }
                }
                LiveInput::Refresh => {}
            }
            let mut query = HashMap::new();
            let html = self.body(r.scope).await?;
            if let Some((act, _)) = &self.dialog {
                query.insert("act".to_owned(), act.key().to_owned());
            }
            Ok(LiveBody {
                html,
                query,
                ..Default::default()
            })
        })
    }
}
