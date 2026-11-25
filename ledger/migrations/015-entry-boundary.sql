CREATE FUNCTION finance_ledger_entry_boundary(tenant bigint, company bigint)
RETURNS bigint LANGUAGE sql STABLE AS $$
  SELECT COALESCE(max(entry_number), 0)
  FROM finance_ledger_entries
  WHERE tenant_id = tenant AND company_id = company
$$;
