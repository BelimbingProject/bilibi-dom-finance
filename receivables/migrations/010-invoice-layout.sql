ALTER TABLE finance_receivables_setups ADD COLUMN invoice_layout jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(invoice_layout) = 'object');
