//! Bilimbi Finance.FixedAssets.Web.AssetLive, including immutable history.
use super::{
    input::{self, Draft},
    *,
};
use crate::{AssetDetail, DISPOSE, REVERSE, VOID};
use bilibi_ui::{ConfirmDialog, FlashMap, InputKind};

pub(super) struct Detail {
    pages: Pages,
    asset_id: i64,
    schedule: ListColumns,
    charges: ListColumns,
    events: ListColumns,
    evidence: super::source_evidence::EvidenceView,
    dispose: Draft,
    reverse: Draft,
    void: Draft,
    pending_void: Option<(Draft, serde_json::Value)>,
    flash: Option<(StatusKind, String)>,
}
#[derive(Template)]
#[template(path = "detail.html")]
struct View<'a> {
    title_actions: Html,
    register_link: Html,
    detail: Option<&'a AssetDetail>,
    unavailable: Html,
    flash: FlashMap<'a>,
    assumptions: Html,
    schedule: Html,
    charges: Html,
    events: Html,
    dispose: Html,
    void: Html,
    reverse: Html,
    confirm: Html,
}
/// Bilimbi's plain header link back to the register.
#[derive(Template)]
#[template(
    source = r#"<a href="/finance/fixed-assets/register">Register</a>"#,
    ext = "html"
)]
struct RegisterLink;
#[derive(Template)]
#[template(path = "assumptions.html")]
struct AssumptionsView<'a> {
    detail: &'a AssetDetail,
    cost: String,
    residual: String,
    method: &'static str,
    heading: String,
    actor: String,
    recorded: Html,
}
impl Detail {
    pub(super) fn new(
        pages: &Pages,
        path: &HashMap<String, String>,
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
                    ("amount", "Scheduled depreciation"),
                    ("accumulated", "Accumulated"),
                    ("carrying", "Carrying value"),
                ],
            )?,
            charges: cols(
                "asset-charges",
                &[
                    ("run", "Run"),
                    ("month", "Month"),
                    ("amount", "Posted depreciation"),
                    ("entry", "Entry"),
                    ("actor", "Actor"),
                    ("recorded", "Recorded"),
                    ("reversal", "Reversal"),
                ],
            )?,
            events: cols(
                "asset-events",
                &[
                    ("action", "Event"),
                    ("effective", "Effective"),
                    ("reason", "Reason"),
                    ("actor", "Actor"),
                    ("recorded", "Recorded"),
                ],
            )?,
            dispose: Draft::default(),
            reverse: Draft::default(),
            void: Draft::default(),
            pending_void: None,
            flash: None,
        })
    }
    async fn form(
        &self,
        s: &Scope,
        op: &str,
        accounts: &[(i64, String, String, bool, String)],
    ) -> Result<Html> {
        let (d, cap, title, reason) = match op {
            "dispose" => (
                &self.dispose,
                DISPOSE,
                "Disposal unavailable",
                "Ask your company administrator for asset disposal permission.",
            ),
            "reverse" => (
                &self.reverse,
                REVERSE,
                "Reversal unavailable",
                "Ask your company administrator for asset reversal permission.",
            ),
            _ => (
                &self.void,
                VOID,
                "Void unavailable",
                "Ask your company administrator for asset void permission.",
            ),
        };
        if !self.pages.allowed(s, cap).await? {
            return withheld(&format!("asset-{op}-withheld"), title, reason);
        }
        let (date_id, date_label, reason_id, verb) = match op {
            "dispose" => (
                "asset_effective_date",
                "Disposal date",
                "asset_reason",
                "Post disposal",
            ),
            "reverse" => (
                "asset-reversal-date",
                "Reversal date",
                "asset-reversal-reason",
                "Post reversal",
            ),
            _ => (
                "asset-void-date",
                "Void date",
                "asset-void-reason",
                "Void acquisition",
            ),
        };
        let prefix = if op == "dispose" {
            "asset".into()
        } else {
            format!("asset-{op}")
        };
        let mut fields = vec![d
            .field("effective_date", date_label, InputKind::Date, true, &prefix)
            .id(date_id)];
        if op == "dispose" {
            fields.extend([
                d.field(
                    "proceeds",
                    "Proceeds (zero for a write-off)",
                    InputKind::Text,
                    true,
                    &prefix,
                ),
                d.field(
                    "settlement_account_id",
                    "Proceeds account (for a sale)",
                    InputKind::Select,
                    false,
                    &prefix,
                )
                .options(
                    std::iter::once(bilibi_ui::Choice::new("Choose an account", ""))
                        .chain(
                            accounts.iter().filter(|a| a.3 && a.2 == "asset").map(|a| {
                                bilibi_ui::Choice::new(&a.1, a.0.to_string()).detail(&a.4)
                            }),
                        )
                        .collect(),
                ),
            ]);
        }
        fields.push(
            d.field("reason", "Reason", InputKind::Text, true, &prefix)
                .id(reason_id),
        );
        render(Operation {
            form_dom: &format!("asset-{op}-form"),
            event_name: op,
            button_dom: &format!("asset-{op}"),
            verb,
            fields,
            help: "",
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
        let (detail, unavailable) = match read {
            Ok(d) => (Some(d), Html::EMPTY),
            Err(e) => (
                None,
                render(
                    EmptyState::missing("Asset unavailable", refusal(e)).id("asset-unavailable"),
                )?,
            ),
        };
        let (mut assumptions, mut schedule, mut charges, mut events) =
            (Html::EMPTY, Html::EMPTY, Html::EMPTY, Html::EMPTY);
        if let Some(d) = &detail {
            assumptions = render(AssumptionsView {
                detail: d,
                cost: d.asset.cost.display(units),
                residual: d.asset.residual_value.display(units),
                heading: format!("#{} {}", d.asset.asset_number, d.asset.description),
                method: input::method(d.asset.assumptions.method),
                actor: actor(
                    d.asset.recorded_by_user_id,
                    d.asset.recorded_by_impersonator_id,
                ),
                recorded: render(Recorded {
                    cell: time_cell(&display, &d.asset.recorded_at)?,
                })?,
            })?;
            if let Some(opening) = &d.opening {
                let fact = |title, value: String| bilibi_ui::Fact {
                    title,
                    value: Html::text(&value),
                    ..Default::default()
                };
                assumptions.push(&render(bilibi_ui::Card {
                    id: "asset-opening",
                    title: "Opening register",
                    body: render(bilibi_ui::Facts {
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
                                opening["opening_date"].as_str().unwrap_or("").to_owned(),
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
            }
            if let Some(opening) = &d.opening {
                assumptions.push(&self.evidence.render(
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
                        .cell("month", Cell::text(r.period.to_string()))
                        .cell("amount", Cell::text(r.amount.display(units)))
                        .cell("accumulated", Cell::text(r.accumulated.display(units)))
                        .cell("carrying", Cell::text(r.carrying_value.display(units)))
                })
                .collect();
            schedule = table(
                "asset-schedule",
                &self.schedule,
                rows,
                "Projected schedule",
                "schedule_grid",
                "",
            )?;
            let mut rows = Vec::new();
            for r in &d.charges {
                let reversal = r
                    .reversal
                    .as_ref()
                    .map(|e| {
                        format!(
                            "Entry {} on {}: {}",
                            e.entry_number
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into()),
                            e.effective_date,
                            e.reason
                        )
                    })
                    .unwrap_or_else(|| "—".into());
                rows.push(
                    FlexRow::new(r.run_id.to_string())
                        .cell("run", Cell::text(r.run_id.to_string()))
                        .cell("month", Cell::text(r.period.to_string()))
                        .cell("amount", Cell::text(r.amount.display(units)))
                        .cell(
                            "entry",
                            Cell::text(
                                r.entry_number
                                    .map(|n| n.to_string())
                                    .unwrap_or_else(|| "—".into()),
                            ),
                        )
                        .cell(
                            "actor",
                            Cell::text(actor(r.recorded_by_user_id, r.recorded_by_impersonator_id)),
                        )
                        .cell("recorded", time_cell(&display, &r.recorded_at)?)
                        .cell("reversal", Cell::text(reversal)),
                );
            }
            charges = table(
                "asset-charges",
                &self.charges,
                rows,
                "Posted charges",
                "charges_grid",
                "No depreciation charges posted.",
            )?;
            let mut rows = Vec::new();
            for e in &d.events {
                let action = match e.action {
                    crate::EventAction::Disposed => "disposed",
                    crate::EventAction::DisposalReversed => "disposal_reversed",
                    crate::EventAction::AcquisitionVoided => "acquisition_voided",
                    crate::EventAction::RunReversed => "run_reversed",
                };
                rows.push(
                    FlexRow::new(e.id.to_string())
                        .cell("action", Cell::text(action))
                        .cell("effective", Cell::text(e.effective_date.to_string()))
                        .cell("reason", Cell::text(e.reason.clone()))
                        .cell("actor", Cell::text(e.recorded_by_user_id.to_string()))
                        .cell("recorded", time_cell(&display, &e.recorded_at)?),
                );
            }
            events = table(
                "asset-events",
                &self.events,
                rows,
                "Asset events",
                "events_grid",
                "No disposal or void events.",
            )?;
        }
        let visible = detail.is_some();
        let title_actions = Html::EMPTY;
        View {title_actions,register_link:render(RegisterLink)?,detail:detail.as_ref(),unavailable,flash:flash_map(&self.flash),assumptions,schedule,charges,events,dispose:if visible {self.form(s,"dispose",&accounts).await?}else{Html::EMPTY},void:if detail.as_ref().is_some_and(|d|!d.voided){self.form(s,"void",&accounts).await?}else{Html::EMPTY},reverse:if visible{self.form(s,"reverse",&accounts).await?}else{Html::EMPTY},confirm:if self.pending_void.is_some(){render(ConfirmDialog {id:"asset-void-confirm",consequence:"Void acquisition",detail:"Depreciation already posted for this asset is reversed; other assets keep theirs. This cannot be undone.",confirm:"Void acquisition",working:"Recording…",confirm_event:"confirm_void",cancel_event:"cancel_void"})?}else{Html::EMPTY}}.render().map_err(|_|Error::Internal)
    }
    fn input(&self, s: &Scope, d: &Draft) -> crate::Result<serde_json::Value> {
        let mut input = d.event_values();
        input["company_id"] = company(s).ok_or(FixedAssetsError::CompanyNotFound)?.into();
        input["asset_id"] = self.asset_id.into();
        Ok(input)
    }
    async fn perform(&self, s: &Scope, credential: &str, op: &str, d: &Draft) -> crate::Result<()> {
        let mut input = self.input(s, d)?;
        let key = if op == "dispose" {
            input["proceeds"] = serde_json::json!(d.value("proceeds"));
            let account = d.value("settlement_account_id").trim();
            if !account.is_empty() {
                input["settlement_account_id"] = account
                    .parse::<i64>()
                    .map_or_else(|_| serde_json::json!(account), |id| serde_json::json!(id));
            }
            "finance.fixed_assets.asset.dispose"
        } else {
            "finance.fixed_assets.disposal.reverse"
        };
        self.pages.submit(s, credential, op, key, input).await?;
        Ok(())
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
            "cancel_void" => {
                self.pending_void = None;
                return Ok(());
            }
            "dispose" | "reverse" | "void" | "confirm_void" => {}
            _ => return Err(Error::Invalid),
        }
        let cap = match event {
            "dispose" => DISPOSE,
            "reverse" => REVERSE,
            _ => VOID,
        };
        if !self.pages.allowed(s, cap).await? {
            self.pending_void = None;
            self.flash = Some((StatusKind::Error, DENIED.into()));
            return Ok(());
        }
        if event == "confirm_void" {
            let Some((mut draft, input)) = self.pending_void.take() else {
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
                Ok(_) => self.flash = Some((StatusKind::Success, "Acquisition voided.".into())),
                Err(e) => {
                    draft.refused(&e);
                    self.flash = Some((StatusKind::Error, refusal(e)));
                }
            }
            self.void = draft;
            return Ok(());
        }
        let mut draft = Draft::posted(v);
        if event == "void" {
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
                Ok(input) => self.pending_void = Some((draft.clone(), input)),
                Err(e) => {
                    draft.refused(&e);
                    self.flash = Some((StatusKind::Error, refusal(e)));
                }
            }
            self.void = draft;
            return Ok(());
        }
        match self.perform(s, credential, event, &draft).await {
            Ok(()) => self.flash = Some((StatusKind::Success, "Recorded in the ledger.".into())),
            Err(e) => {
                draft.refused(&e);
                self.flash = Some((StatusKind::Error, refusal(e)));
            }
        }

        match event {
            "dispose" => self.dispose = draft,
            "reverse" => self.reverse = draft,
            _ => self.void = draft,
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
                            self.dispose = Draft::default();
                            self.reverse = Draft::default();
                            self.void = Draft::default();
                        }
                    }
                }
                LiveInput::Refresh => {}
            }
            Ok(LiveBody {
                html: self.body(r.scope).await?,
                ..Default::default()
            })
        })
    }
}
