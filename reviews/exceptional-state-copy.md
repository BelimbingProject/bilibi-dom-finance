# Finance exceptional-state copy review

Reviewed on 28 October 2026 against the mounted application at `http://127.0.0.1:31897`, `http://127.0.0.1:31899` and `http://127.0.0.1:31901`, each the exact `APP_ORIGIN` of a separate disposable fixture. The recorder, reviewer and read-only viewer used the fixture accounts; the narrow check used a 390 × 844 viewport. Snapshots and screenshots are retained with the task evidence. The routes below are under `/finance`.

## Changes

| Page and state | Before | After and browser evidence |
| --- | --- | --- |
| `einvoice/documents/4`, expired cancellation | “The 72-hour cancellation window has closed; issue a credit note instead.” | “The cancellation window has closed. Issue the appropriate credit, debit or refund note e-Invoice linked to the original.” A valid fixture document with a validation time 73 hours earlier was taken through both cancellation dialogs; the refusal appeared and the status remained valid. |
| E-invoice detail, authority refuses an expired cancellation | “The authority refused: the cancellation window has closed.” | The same linked-note guidance follows the authority refusal. This specific authority-refusal branch was source reviewed; the browser case above exercised the local expired-window refusal. |
| `einvoice/documents/6`, buyer rejection request | “The buyer requested rejection. Cancel within the 72-hour window, or issue a credit note.” | The warning now offers the appropriate linked credit, debit or refund note as an alternative. The local authority returned a buyer rejection request and the browser displayed the warning. |
| `einvoice/documents/5`, cancellation dialogs | “The authority marks the document cancelled; it cannot be undone. Its ledger entry is reversed.” | Both dialogs say cancellation is requested and that the ledger reversal is attempted after acceptance. Both rendered in the browser. |
| `einvoice/documents/5`, lost cancellation response | “The authority could not be reached. Try again later.” | “The authority did not confirm cancellation. Refresh the status before trying again.” The local authority accepted the request but delayed its response beyond the 30-second transport timeout. The page remained valid; a second attempt checked the authority first, recorded cancelled and sent no second cancellation request; the page says “The tax authority had already cancelled this document. Its status is now updated.” (`cancel_document` answers the updated document as a success in this case, where it used to refuse with `NotCancellable`). When that pre-cancel status read is rate limited or cannot be read, nothing is sent and the page says so. |
| E-invoice detail, no longer cancellable | “Only a validated document can be cancelled.” | “Only a valid document or one with a rejection request can be cancelled.” This distinguishes a previously validated but already cancelled document. |
| `sst/returns/1`, open and finalised return | No explanation of the external filing and payment boundary at Part A. | “Finalising fixes these figures. It does not submit the return or pay the tax.” A sales-tax return was prepared, given an adjustment and finalised in the browser; the wording remained visible, including at 390 × 844. |

The linked note language follows [IRBM's cancellation API](https://sdk.myinvois.hasil.gov.my/einvoicingapi/03-cancel-document/) and [e-Invoice FAQ](https://www.hasil.gov.my/media/0xqitc2t/lhdnm-e-invoice-general-faqs.pdf). The SST wording follows the practitioner review on issue 71: preparation and finalisation do not establish submission or payment. Cancellation status and a ledger reversal are separate outcomes.

## Exceptional states left unchanged

| Page | Browser evidence |
| --- | --- |
| `ledger/books/{company_id}` | Switched to Acme Trading, whose book was unopened; the open-book form named the company and explained the fixed currency and approval policy. |
| `ledger/fiscal-years` | Closed then locked January in the disposable company; the lock confirmation said the period can never be reopened. |
| `receivables/receipts/1` and `receivables/invoices/1` | Reversed a posted receipt; the invoice showed its balance owing again and the receipt as reversed. The reversal dialog explained the consequence and earliest date. |
| `receivables/invoices/1`, bad-debt journal | Proposed a write-off, approved it as a different reviewer, then reversed it. The dialogs kept the collection-evidence reminder and stated that no tax refund or filing was recorded. |
| `reconciliation/accounts/1/matching` | Imported a CSV, accepted a suggested match for a 1,670.00 receipt and saw it in standing matches. |
| `reconciliation/reconciliations/1` | Opened a reconciliation with matching statement and ledger balances; finalisation by its preparer was refused under separation of duties, then a different reviewer finalised it. |
| `fixed-assets/runs` and `fixed-assets/register/1` | Recorded and reversed a depreciation run; the register distinguished projected schedules from posted charges. |
| `payroll` | Reversed a historical posting while People was unmounted. An earlier entry date was refused; a later date posted the reversal and retained the original. |
| `payables/bills/1` and attachment panel | The draft file showed its minimum retention date and legal hold; the read-only viewer saw the file without replace or remove controls. |
| `sst/setup` and `sst` | Registered sales tax and prepared a return; the setup, period and empty-return states identified what remained to be done. |
| `einvoice/documents/5` | A timed-out cancellation did not change the displayed status until the authority confirmed it; the subsequent checked attempt displayed the confirmed status and the “already cancelled” success message. |
| `einvoice/documents/1` | A separate cancellation received an acceptance from the local authority and the page reported the confirmed cancellation. Its posting had failed, so this does not prove a ledger reversal. |
| `receivables/invoices/2` | The issued invoice listed its linked credit note under receipts and credits. |
| `payables/bills/37` and `payables/bills/73` | The approved and posted bills retained their source documents alongside the draft bill inspected above. |

## Route inventory

The following mounted routes were opened in the browser as the recorder and their visible initial states were reviewed; no copy change was made on these pages. Record pages and action states are listed separately above. A route's initial state is not evidence of every permission or record combination.

| Area | Unchanged routes under `/finance` |
| --- | --- |
| Ledger | `ledger/books`, `ledger/accounts`, `ledger/accounts/1`, `ledger/account-groups`, `ledger/journals`, `ledger/fiscal-years`, `ledger/trial-balance` |
| Receivables | `receivables/customers`, `receivables/customers/1`, `receivables/invoices`, `receivables/receipts`, `receivables/aging`, `receivables/settings` |
| Payables | `payables/bills`, `payables/payments`, `payables/payments/new`, `payables/aged`, `payables/setup` |
| Bank | `reconciliation`, `reconciliation/accounts/1`, `reconciliation/accounts/1/matching` |
| Reports | `setup`, `reports`, `reports/sections`, `reports/controls`, `reports/approval-exceptions`; `reports/accounts/1` showed “Account not found” |
| Fixed assets | `fixed-assets/register`, `fixed-assets/categories`, `fixed-assets/runs` |
| Payroll | `payroll`, `payroll/setup` |
| SST | `sst`, `sst/setup` |
| E-invoice | `einvoice/documents`, `einvoice/consolidation`, `einvoice/reconciliation`, `einvoice/setup` |
| Attachments | `attachments` |
