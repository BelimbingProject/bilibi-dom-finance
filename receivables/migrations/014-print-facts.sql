ALTER TABLE finance_receivables_documents
  ADD COLUMN print_facts jsonb,
  ADD CONSTRAINT finance_receivables_documents_print_facts
    CHECK (print_facts IS NULL OR (status = 'issued' AND jsonb_typeof(print_facts) = 'object'));
