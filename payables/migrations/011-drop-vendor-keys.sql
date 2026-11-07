-- A vendor's bills, payments and aged balances are grouped under its own key,
-- `vendor:{id}`, which names the vendor without a table. The table that
-- matched old supplier keys to a vendor (and held each vendor's own key) is
-- gone with the feature: Bilibi has no production users to carry old keys for.
DROP TABLE finance_payables_vendor_keys;
DROP FUNCTION finance_payables_vendor_key_guard();
