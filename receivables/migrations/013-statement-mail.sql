ALTER TABLE finance_receivables_mail ALTER COLUMN document_id DROP NOT NULL;
ALTER TABLE finance_receivables_mail ADD COLUMN customer_id bigint;
ALTER TABLE finance_receivables_mail ADD COLUMN statement jsonb;
ALTER TABLE finance_receivables_mail ADD CONSTRAINT finance_receivables_mail_source CHECK (
    (document_id IS NOT NULL AND customer_id IS NULL AND statement IS NULL) OR
    (document_id IS NULL AND customer_id IS NOT NULL AND statement IS NOT NULL AND jsonb_typeof(statement) = 'object')
);
ALTER TABLE finance_receivables_mail ADD FOREIGN KEY (customer_id, company_id) REFERENCES finance_receivables_customers(id, company_id);
