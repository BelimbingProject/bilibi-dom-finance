-- A bank account is added with its name and ledger account only; the columns
-- of its bank's export are mapped on the first statement import, which keeps
-- the mapping with the account in the same transaction. Until then the
-- account's `import_mapping` is NULL. A statement still always records the
-- mapping it was read through, so its column stays NOT NULL.
ALTER TABLE finance_reconciliation_bank_accounts
  ALTER COLUMN import_mapping DROP NOT NULL;
