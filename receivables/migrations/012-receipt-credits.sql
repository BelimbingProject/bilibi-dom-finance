-- A receipt may keep an amount not applied to any invoice (a deposit or an
-- overpayment) as a customer credit. The credit is a document of its own kind,
-- settled by the same applications and refunds as an opening customer credit.
ALTER TABLE finance_receivables_setups ADD COLUMN customer_credit_account_id bigint;
ALTER TABLE finance_receivables_setup_changes DROP CONSTRAINT finance_receivables_setup_changes_field;
ALTER TABLE finance_receivables_setup_changes ADD CONSTRAINT finance_receivables_setup_changes_field
  CHECK (field IN ('receivable_account_id', 'income_account_id', 'customer_credit_account_id'));

ALTER TABLE finance_receivables_documents DROP CONSTRAINT finance_receivables_documents_kind;
ALTER TABLE finance_receivables_documents ADD CONSTRAINT finance_receivables_documents_kind
  CHECK (kind IN ('invoice', 'credit_note', 'opening_credit', 'customer_credit'));

-- A receipt's credit carries its receipt's series, number and reference.
CREATE OR REPLACE FUNCTION finance_receivables_document_series_kind() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM finance_receivables_series s
                 WHERE s.id = NEW.series_id AND s.company_id = NEW.company_id AND s.tenant_id = NEW.tenant_id
                   AND s.kind = CASE NEW.kind WHEN 'opening_credit' THEN 'invoice'
                                              WHEN 'customer_credit' THEN 'payment' ELSE NEW.kind END) THEN
    RAISE EXCEPTION 'document series must match its kind' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NEW;
END $$;

-- The link is a table of its own: the document and payment evidence field
-- lists are frozen.
CREATE TABLE finance_receivables_receipt_credits (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  payment_id bigint NOT NULL, document_id bigint NOT NULL,
  amount numeric(20, 4) NOT NULL CHECK (amount > 0),
  UNIQUE (payment_id), UNIQUE (document_id),
  FOREIGN KEY (payment_id, company_id) REFERENCES finance_receivables_payments (id, company_id),
  FOREIGN KEY (document_id, company_id) REFERENCES finance_receivables_documents (id, company_id)
);
CREATE INDEX finance_receivables_receipt_credits_scope
  ON finance_receivables_receipt_credits (tenant_id, company_id);
CREATE TRIGGER finance_receivables_receipt_credits_kept BEFORE UPDATE OR DELETE ON finance_receivables_receipt_credits
  FOR EACH ROW EXECUTE FUNCTION finance_receivables_refuse_change();
CREATE TRIGGER finance_receivables_receipt_credits_retain BEFORE TRUNCATE ON finance_receivables_receipt_credits
  FOR EACH STATEMENT EXECUTE FUNCTION finance_receivables_refuse_change();

CREATE FUNCTION finance_receivables_receipt_credit_shape() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM finance_receivables_documents d
    JOIN finance_receivables_payments p ON p.id = NEW.payment_id
    WHERE d.id = NEW.document_id AND d.tenant_id = NEW.tenant_id AND p.tenant_id = NEW.tenant_id
      AND d.kind = 'customer_credit' AND d.status = 'issued'
      AND d.customer_id = p.customer_id AND d.currency_code = p.currency_code
      AND d.total = NEW.amount AND d.issue_date = p.payment_date
      AND d.series_id = p.series_id AND d.document_number = p.payment_number
      AND d.document_reference = p.payment_reference
      AND d.ledger_entry_id = p.ledger_entry_id AND d.ledger_entry_number = p.ledger_entry_number
  ) THEN
    RAISE EXCEPTION 'a receipt credit is the unapplied amount of its own receipt'
      USING ERRCODE = 'check_violation';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER finance_receivables_receipt_credit_shape BEFORE INSERT ON finance_receivables_receipt_credits
  FOR EACH ROW EXECUTE FUNCTION finance_receivables_receipt_credit_shape();

CREATE FUNCTION finance_receivables_customer_credit_source() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  -- The link names an issued credit only, so no draft of this kind commits.
  IF NEW.kind = 'customer_credit' AND NOT EXISTS (
    SELECT 1 FROM finance_receivables_receipt_credits k
    WHERE k.tenant_id = NEW.tenant_id AND k.company_id = NEW.company_id AND k.document_id = NEW.id
  ) THEN
    RAISE EXCEPTION 'a customer credit requires the receipt it came from' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER finance_receivables_customer_credit_source AFTER INSERT OR UPDATE ON finance_receivables_documents
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance_receivables_customer_credit_source();

-- What a receipt does not apply to invoices is its credit, to the cent.
CREATE OR REPLACE FUNCTION finance_receivables_check_payment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  checked_id bigint;
BEGIN
  IF TG_TABLE_NAME = 'finance_receivables_payments' THEN
    checked_id := NEW.id;
  ELSE
    checked_id := NEW.payment_id;
  END IF;

  IF (SELECT p.amount
        - COALESCE((SELECT sum(a.amount) FROM finance_receivables_allocations a WHERE a.payment_id = p.id), 0)
        - COALESCE((SELECT k.amount FROM finance_receivables_receipt_credits k WHERE k.payment_id = p.id), 0)
      FROM finance_receivables_payments p WHERE p.id = checked_id) <> 0 THEN
    RAISE EXCEPTION 'payment % is not its allocations plus its credit', checked_id
      USING ERRCODE = 'check_violation';
  END IF;

  IF EXISTS (
    SELECT 1 FROM finance_receivables_allocations a
    WHERE a.payment_id = checked_id
      AND finance_receivables_outstanding(a.document_id) < 0
  ) THEN
    RAISE EXCEPTION 'payment % settles an invoice beyond what it owes', checked_id
      USING ERRCODE = 'check_violation';
  END IF;

  RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER finance_receivables_receipt_credits_settled AFTER INSERT ON finance_receivables_receipt_credits
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION finance_receivables_check_payment();

-- Issuing numbers a document from its series, except a receipt's credit,
-- which takes its receipt's number (checked by the link above).
CREATE OR REPLACE FUNCTION finance_receivables_guard_document() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  credited finance_receivables_documents%ROWTYPE;
BEGIN
  IF OLD.status <> 'draft' THEN
    RAISE EXCEPTION 'finance receivables document % is %: it is never changed; issue a credit note',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.kind <> OLD.kind THEN
    RAISE EXCEPTION 'a finance receivables document keeps its company and kind'
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.status = 'discarded' THEN
    NEW.discarded_at := clock_timestamp() AT TIME ZONE 'UTC';
  ELSIF NEW.status = 'issued' THEN
    IF NEW.kind <> 'customer_credit' AND NEW.document_number IS DISTINCT FROM COALESCE(
      (SELECT max(document_number) FROM finance_receivables_documents
       WHERE series_id = NEW.series_id), 0) + 1 THEN
      RAISE EXCEPTION 'document % is not the next number in series %',
        NEW.document_number, NEW.series_id
        USING ERRCODE = 'check_violation';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM finance_receivables_document_lines WHERE document_id = NEW.id)
      OR NEW.subtotal <> (SELECT sum(amount) FROM finance_receivables_document_lines
                          WHERE document_id = NEW.id)
      OR NEW.tax_total <> (SELECT sum(tax_amount) FROM finance_receivables_document_lines
                           WHERE document_id = NEW.id)
      OR NEW.tax_total <> COALESCE((SELECT sum(tax_amount)
                                    FROM finance_receivables_document_tax_lines
                                    WHERE document_id = NEW.id), 0) THEN
      RAISE EXCEPTION 'document % totals do not equal its lines', NEW.id
        USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.kind = 'credit_note' THEN
      SELECT * INTO credited FROM finance_receivables_documents
      WHERE id = NEW.credited_document_id;

      IF credited.kind <> 'invoice' OR credited.status <> 'issued'
        OR credited.customer_id <> NEW.customer_id THEN
        RAISE EXCEPTION 'a credit note credits an issued invoice of the same customer'
          USING ERRCODE = 'check_violation';
      END IF;

      IF NEW.receivable_account_id IS DISTINCT FROM credited.receivable_account_id THEN
        RAISE EXCEPTION 'a credit note posts to the receivable account of its invoice'
          USING ERRCODE = 'check_violation';
      END IF;

      IF finance_receivables_outstanding(credited.id) < NEW.total THEN
        RAISE EXCEPTION 'credit note % exceeds what invoice % still owes', NEW.id, credited.id
          USING ERRCODE = 'check_violation';
      END IF;
    END IF;

    NEW.issued_at := clock_timestamp() AT TIME ZONE 'UTC';
  END IF;

  RETURN NEW;
END
$$;

-- A reversed receipt takes its credit with it from the reversal date.
CREATE OR REPLACE FUNCTION finance_receivables_residual(t bigint,c bigint,document bigint,day date,posting bigint,settlement bigint) RETURNS numeric LANGUAGE sql STABLE AS $$
 SELECT d.total
 - COALESCE((SELECT sum(n.total) FROM finance_receivables_documents n WHERE n.tenant_id=t AND n.company_id=c AND n.credited_document_id=d.id AND n.status='issued' AND n.issue_date<=day AND n.ledger_entry_number<=posting),0)
 - COALESCE((SELECT sum(a.amount) FROM finance_receivables_allocations a JOIN finance_receivables_payments p ON p.id=a.payment_id AND p.tenant_id=t AND p.company_id=c WHERE a.tenant_id=t AND a.company_id=c AND a.document_id=d.id AND p.payment_date<=day AND p.ledger_entry_number<=posting AND NOT EXISTS(SELECT 1 FROM finance_receivables_payment_reversals x WHERE x.tenant_id=t AND x.company_id=c AND x.payment_id=p.id AND x.reversal_date<=day AND x.ledger_entry_number<=posting)),0)
 - COALESCE((SELECT sum(w.amount) FROM finance_receivables_write_offs w WHERE w.tenant_id=t AND w.company_id=c AND w.document_id=d.id AND EXISTS(SELECT 1 FROM finance_receivables_write_off_events e WHERE e.tenant_id=t AND e.company_id=c AND e.write_off_id=w.id AND e.event='approved' AND e.effective_date<=day AND e.ledger_entry_number<=posting) AND NOT EXISTS(SELECT 1 FROM finance_receivables_write_off_events e WHERE e.tenant_id=t AND e.company_id=c AND e.write_off_id=w.id AND e.event='reversed' AND e.effective_date<=day AND e.ledger_entry_number<=posting)),0)
 - COALESCE((SELECT sum(CASE WHEN e.kind='reverse' THEN -e.amount ELSE e.amount END) FROM finance_receivables_settlements e WHERE e.tenant_id=t AND e.company_id=c AND (e.credit_document_id=d.id OR e.target_document_id=d.id) AND e.effective_date<=day AND e.settlement_number<=settlement AND (e.entry_number IS NULL OR e.entry_number<=posting)),0)
 - COALESCE((SELECT d.total FROM finance_receivables_receipt_credits k JOIN finance_receivables_payment_reversals x ON x.payment_id=k.payment_id AND x.tenant_id=t AND x.company_id=c WHERE k.tenant_id=t AND k.company_id=c AND k.document_id=d.id AND x.reversal_date<=day AND x.ledger_entry_number<=posting),0)
 FROM finance_receivables_documents d WHERE d.tenant_id=t AND d.company_id=c AND d.id=document
$$;

-- A settlement settles an opening credit (through its source item) or a
-- receipt's credit (no item).
ALTER TABLE finance_receivables_settlements ALTER COLUMN credit_item_id DROP NOT NULL;
CREATE OR REPLACE FUNCTION finance_receivables_settlement_shape() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 -- This lock is also taken by Ledger::write; serialize even an accidental
 -- direct insert before checking the stream's next number.
 PERFORM id FROM companies WHERE tenant_id=NEW.tenant_id AND id=NEW.company_id FOR UPDATE;
 IF NEW.settlement_number<>(SELECT COALESCE(max(settlement_number),0)+1 FROM finance_receivables_settlements WHERE tenant_id=NEW.tenant_id AND company_id=NEW.company_id) THEN
 RAISE EXCEPTION 'settlement number must be next for this company' USING ERRCODE='check_violation'; END IF;
 IF NEW.credit_item_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM finance_receivables_opening_items i WHERE i.tenant_id=NEW.tenant_id AND i.company_id=NEW.company_id AND i.id=NEW.credit_item_id AND i.document_id=NEW.credit_document_id AND i.source_facts->>'item_kind'='customer_credit') THEN
 RAISE EXCEPTION 'settlement requires an opening credit source' USING ERRCODE='check_violation'; END IF;
 IF NEW.credit_item_id IS NULL AND NOT EXISTS(SELECT 1 FROM finance_receivables_documents d WHERE d.tenant_id=NEW.tenant_id AND d.company_id=NEW.company_id AND d.id=NEW.credit_document_id AND d.kind='customer_credit' AND d.status='issued') THEN
 RAISE EXCEPTION 'settlement requires a customer credit' USING ERRCODE='check_violation'; END IF;
 IF NEW.kind='reverse' AND NOT EXISTS(SELECT 1 FROM finance_receivables_settlements e WHERE e.tenant_id=NEW.tenant_id AND e.company_id=NEW.company_id AND e.id=NEW.reverses_settlement_id AND e.kind<>'reverse' AND e.settlement_number<NEW.settlement_number AND e.credit_item_id IS NOT DISTINCT FROM NEW.credit_item_id AND e.credit_document_id=NEW.credit_document_id AND e.target_document_id IS NOT DISTINCT FROM NEW.target_document_id AND e.cash_account_id IS NOT DISTINCT FROM NEW.cash_account_id AND e.amount=NEW.amount AND e.effective_date<=NEW.effective_date AND (e.entry_id IS NULL)=(NEW.entry_id IS NULL)) THEN
 RAISE EXCEPTION 'reversal must restore its original settlement' USING ERRCODE='check_violation'; END IF;
 RETURN NEW;
END $$;

-- The facade checks every dated balance under the company lock. The database
-- refuses whatever would still leave a credit or an invoice below zero.
CREATE FUNCTION finance_receivables_settlement_capacity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF finance_receivables_outstanding(NEW.credit_document_id) < 0 THEN
 RAISE EXCEPTION 'settlement % uses more than credit % has left', NEW.id, NEW.credit_document_id USING ERRCODE='check_violation'; END IF;
 IF NEW.target_document_id IS NOT NULL AND finance_receivables_outstanding(NEW.target_document_id) < 0 THEN
 RAISE EXCEPTION 'settlement % settles invoice % beyond what it owes', NEW.id, NEW.target_document_id USING ERRCODE='check_violation'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER finance_receivables_settlement_capacity AFTER INSERT ON finance_receivables_settlements
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance_receivables_settlement_capacity();

-- A receipt whose credit has been applied or refunded is reversed only after
-- those settlements are.
CREATE FUNCTION finance_receivables_reversal_credit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM finance_receivables_receipt_credits k WHERE k.tenant_id=NEW.tenant_id AND k.company_id=NEW.company_id AND k.payment_id=NEW.payment_id AND finance_receivables_outstanding(k.document_id) < 0) THEN
 RAISE EXCEPTION 'payment % has a credit that is still applied or refunded', NEW.payment_id USING ERRCODE='check_violation'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER finance_receivables_reversal_credit AFTER INSERT ON finance_receivables_payment_reversals
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance_receivables_reversal_credit();
