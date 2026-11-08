-- A retained source-period schedule is never edited. A mistaken one is
-- withdrawn, or replaced by a new schedule of the same kind, with a reason;
-- the original stays as evidence. Each schedule takes one correction at most,
-- and a schedule replaces at most one other.
CREATE TABLE finance_reports_source_period_corrections (
  id bigserial PRIMARY KEY,
  tenant_id bigint NOT NULL,
  company_id bigint NOT NULL,
  schedule_id bigint NOT NULL REFERENCES finance_reports_source_periods (id),
  replacement_id bigint REFERENCES finance_reports_source_periods (id),
  reason text NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 500),
  recorded_by_user_id bigint NOT NULL,
  recorded_by_impersonator_id bigint,
  recorded_at timestamp(6) NOT NULL DEFAULT (clock_timestamp() AT TIME ZONE 'UTC'),
  UNIQUE (tenant_id, company_id, schedule_id),
  UNIQUE (tenant_id, company_id, replacement_id),
  CHECK (replacement_id IS NULL OR replacement_id <> schedule_id)
);
CREATE INDEX finance_reports_source_period_corrections_scope ON finance_reports_source_period_corrections (tenant_id, company_id, id);
CREATE TRIGGER finance_reports_source_period_correction_immutable BEFORE UPDATE OR DELETE ON finance_reports_source_period_corrections
  FOR EACH ROW EXECUTE FUNCTION finance_reports_keep_source_period();
CREATE TRIGGER finance_reports_source_period_correction_no_truncate BEFORE TRUNCATE ON finance_reports_source_period_corrections
  FOR EACH STATEMENT EXECUTE FUNCTION finance_reports_keep_source_period();
