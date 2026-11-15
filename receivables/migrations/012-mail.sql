CREATE TABLE finance_receivables_mail (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    document_id bigint NOT NULL,
    recipient text NOT NULL,
    subject text NOT NULL,
    body text NOT NULL,
    status text NOT NULL CHECK (status IN ('queued', 'sending', 'sent', 'failed')),
    inserted_at timestamp without time zone NOT NULL DEFAULT (now() AT TIME ZONE 'UTC'),
    updated_at timestamp without time zone NOT NULL DEFAULT (now() AT TIME ZONE 'UTC'),
    FOREIGN KEY (tenant_id, company_id) REFERENCES companies(tenant_id, id),
    FOREIGN KEY (document_id, company_id) REFERENCES finance_receivables_documents(id, company_id)
);
