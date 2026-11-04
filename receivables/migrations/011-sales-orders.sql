-- Finance owns this event copy. Commerce is optional and has no schema edge.
CREATE TABLE finance_receivables_sales_orders (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    order_id bigint NOT NULL CHECK (order_id > 0),
    order_number text NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    status text NOT NULL CHECK (status IN ('confirmed','cancelled')),
    part_count integer NOT NULL CHECK (part_count BETWEEN 1 AND 100),
    facts jsonb NOT NULL,
    issue_date date NOT NULL,
    document_id bigint,
    waiting_reason text NOT NULL DEFAULT 'Waiting for all order lines.',
    attempted_at timestamp without time zone,
    UNIQUE (tenant_id,company_id,order_id),
    UNIQUE (tenant_id,company_id,id),
    UNIQUE (tenant_id,company_id,document_id),
    FOREIGN KEY (tenant_id,company_id) REFERENCES companies(tenant_id,id),
    FOREIGN KEY (tenant_id,company_id,document_id) REFERENCES finance_receivables_documents(tenant_id,company_id,id)
);
CREATE INDEX finance_receivables_sales_orders_pending ON finance_receivables_sales_orders(tenant_id,attempted_at NULLS FIRST,id) WHERE document_id IS NULL AND status='confirmed';
CREATE TABLE finance_receivables_sales_order_parts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL,
    company_id bigint NOT NULL,
    sales_order_id bigint NOT NULL,
    part_index integer NOT NULL CHECK (part_index BETWEEN 1 AND 100),
    event_key text NOT NULL,
    payload jsonb NOT NULL,
    UNIQUE (tenant_id,company_id,sales_order_id,part_index),
    UNIQUE (tenant_id,event_key),
    FOREIGN KEY (tenant_id,company_id,sales_order_id) REFERENCES finance_receivables_sales_orders(tenant_id,company_id,id)
);
CREATE FUNCTION finance_receivables_guard_sales_order() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR NEW.tenant_id<>OLD.tenant_id OR NEW.company_id<>OLD.company_id OR NEW.order_id<>OLD.order_id
       OR NEW.order_number<>OLD.order_number OR NEW.facts<>OLD.facts OR NEW.part_count<>OLD.part_count
       OR NEW.issue_date<>OLD.issue_date OR (OLD.document_id IS NOT NULL AND NEW.document_id IS DISTINCT FROM OLD.document_id)
       OR NEW.revision<OLD.revision OR (OLD.status='cancelled' AND NEW.status<>'cancelled') THEN
        RAISE EXCEPTION 'sales order source is permanent' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER finance_receivables_sales_order_guard BEFORE UPDATE OR DELETE ON finance_receivables_sales_orders FOR EACH ROW EXECUTE FUNCTION finance_receivables_guard_sales_order();
CREATE TRIGGER finance_receivables_sales_order_parts_guard BEFORE UPDATE OR DELETE ON finance_receivables_sales_order_parts FOR EACH ROW EXECUTE FUNCTION finance_receivables_refuse_change();

CREATE FUNCTION finance_receivables_document_source(p_tenant bigint,p_company bigint,p_document bigint) RETURNS text LANGUAGE sql STABLE AS $$
SELECT CASE
    WHEN EXISTS(SELECT 1 FROM finance_receivables_opening_items WHERE tenant_id=p_tenant AND company_id=p_company AND document_id=p_document) THEN 'opening'
    WHEN EXISTS(SELECT 1 FROM finance_receivables_sales_orders WHERE tenant_id=p_tenant AND company_id=p_company AND document_id=p_document) THEN 'sales_order'
    ELSE 'ordinary' END
$$;
