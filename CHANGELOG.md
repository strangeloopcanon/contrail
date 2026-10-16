# Changelog

## contrail-cli 0.1.8 / contrails 0.1.7

- Include supported Antigravity native histories in default all-app backups. `backup --app antigravity` selects an explicitly partial backup; `catalog --app antigravity` filters retained files. Inventory summaries report Antigravity counts.
- Preserve conversation protobufs, history-only SQLite snapshots, conversation artifacts, annotations, recordings and summaries. Exclude account state, configuration and credentials. Unknown history schemas fail closed.
- Add Antigravity files to existing all-app incremental chains without discarding earlier history. Recovery uses the existing catalog, restore-plan and extract commands; native app import/resume remains untested.
- Bound SQLite projection scratch to two database copies and compact disposable offline snapshots without deletion journals. Keep live database settings unchanged.
- Accept the Codex thread-history `root_turn_id` column while retaining unknown-column rejection.
- Write compact manifests and refresh verification receipts when generating a cloud handoff, so the upload list and local release checks bind the same manifest bytes.

The archive format and cloud verification policies are unchanged. Native history deletion remains disabled. No new dependencies are required.
