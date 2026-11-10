-- Emailing an issued invoice. The company's choice lives with its setup (off
-- until someone turns it on); each send is one row that says who asked, when,
-- to whom, with which words and which PDF, and how the delivery ended.
ALTER TABLE finance_receivables_setups ADD COLUMN invoice_email jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(invoice_email) = 'object');

CREATE TABLE finance_receivables_document_emails (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    document_id bigint NOT NULL,
    to_email varchar(254) NOT NULL CHECK (position('@' IN to_email) > 1),
    from_name varchar(200) NOT NULL,
    reply_to varchar(254),
    subject varchar(200) NOT NULL CHECK (length(trim(subject)) > 0),
    message varchar(5000) NOT NULL CHECK (length(trim(message)) > 0),
    attachment_name varchar(255) NOT NULL,
    -- Known once the PDF is made for this send: its size and SHA-256.
    attachment_bytes bigint CHECK (attachment_bytes > 0),
    attachment_sha256 char(64),
    message_id varchar(254),
    state varchar(16) NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','sent','failed','cancelled')),
    reason varchar(64),
    requested_by_user_id bigint NOT NULL,
    requested_at timestamp(6) NOT NULL DEFAULT (now() AT TIME ZONE 'UTC'),
    settled_at timestamp(6),
    CHECK ((state = 'queued') = (settled_at IS NULL)),
    CHECK (state <> 'sent' OR (attachment_bytes IS NOT NULL AND attachment_sha256 IS NOT NULL AND message_id IS NOT NULL)),
    FOREIGN KEY (tenant_id, company_id) REFERENCES companies(tenant_id, id),
    FOREIGN KEY (document_id, company_id) REFERENCES finance_receivables_documents(id, company_id)
);
CREATE INDEX finance_receivables_document_emails_document ON finance_receivables_document_emails(tenant_id, company_id, document_id, id);

-- Only an issued invoice is emailed, a send's words never change, and a
-- delivery ends once.
CREATE FUNCTION finance_receivables_guard_document_email() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP = 'DELETE' THEN
  RAISE EXCEPTION 'an invoice email record is kept';
 ELSIF TG_OP = 'INSERT' THEN
  IF NEW.state <> 'queued' OR NOT EXISTS (SELECT 1 FROM finance_receivables_documents d WHERE d.id=NEW.document_id AND d.tenant_id=NEW.tenant_id AND d.company_id=NEW.company_id AND d.kind='invoice' AND d.status='issued') THEN
   RAISE EXCEPTION 'only an issued invoice can be emailed';
  END IF;
 ELSIF OLD.state <> 'queued' OR NEW.state = 'queued'
    OR (to_jsonb(NEW) - ARRAY['state','reason','settled_at','attachment_bytes','attachment_sha256','message_id'])
       <> (to_jsonb(OLD) - ARRAY['state','reason','settled_at','attachment_bytes','attachment_sha256','message_id']) THEN
  RAISE EXCEPTION 'an invoice email is settled once and its content never changes';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER finance_receivables_document_email_guard BEFORE INSERT OR UPDATE OR DELETE ON finance_receivables_document_emails FOR EACH ROW EXECUTE FUNCTION finance_receivables_guard_document_email();
