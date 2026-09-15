## Default Permission

Open and edit Loom's app-owned default project or a user-selected project. Generation and hosted-provider authority are separate permissions.

#### This default permission set includes the following:

- `allow-signal-request`
- `allow-cabal-network-get`
- `allow-cabal-network-set`
- `allow-cabal-snapshot`
- `allow-cabal-share`
- `allow-cabal-join`
- `allow-cabal-open`
- `allow-cabal-workspace`
- `allow-cabal-edit`
- `allow-cabal-revoke`
- `allow-cabal-recover`
- `allow-project-open-default`
- `allow-project-prepare-open`
- `allow-project-prepare-open-path`
- `allow-project-drop-directories`
- `allow-project-commit-open`
- `allow-project-discard-open`
- `allow-project-close`
- `allow-project-current`
- `allow-project-recover`
- `allow-document-create`
- `allow-workspace-template-get`
- `allow-workspace-template-enable`
- `allow-document-rename`
- `allow-document-delete`
- `allow-attachment-ingest`
- `allow-attachment-import-choose`
- `allow-attachment-import-paths`
- `allow-attachment-reveal-original`
- `allow-import-text-sources`
- `allow-attachment-import-batch-choose`
- `allow-document-context-list`
- `allow-document-context-add`
- `allow-document-context-add-many`
- `allow-document-context-remove`
- `allow-document-context-text-get`
- `allow-document-context-text-set`
- `allow-document-context-snapshot-set`
- `allow-co-writer-list`
- `allow-co-writer-save`
- `allow-co-writer-apply`
- `allow-co-writer-delete`
- `allow-speech-input-capabilities`
- `allow-speech-input-status`
- `allow-document-open`
- `allow-document-import-external`
- `allow-shader-preview`
- `allow-document-checkpoint`
- `allow-document-export-choose`
- `allow-document-reveal`
- `allow-document-draft-upsert`
- `allow-document-draft-clear`
- `allow-document-reconciliation-preview`
- `allow-document-reconcile-apply`
- `allow-build-model-policy-get`
- `allow-model-catalog-list`
- `allow-model-list`
- `allow-model-choose`
- `allow-model-download-status`
- `allow-model-download-list`
- `allow-completion-snapshot`
- `allow-branch-page`
- `allow-branch-get`
- `allow-branch-body`
- `allow-weave-status`
- `allow-application-close`
- `allow-application-close-abort`
- `allow-application-close-pending`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`loom:peer-compute`

</td>
<td>

Review device-specific idle model grants and explicitly prepare, submit, check, or cancel exact peer jobs. Grants bind the verified model and current cabal membership. This does not grant manuscript or local-tool authority.

</td>
</tr>

<tr>
<td>

`loom:local-generation`

</td>
<td>

Load a local model and create, cancel, or retain private generation branches. This cannot promote model text into the active manuscript.

</td>
</tr>

<tr>
<td>

`loom:manuscript-promotion`

</td>
<td>

Promote an explicitly selected private candidate into the active manuscript through Loom's source-bound store command.

</td>
</tr>

<tr>
<td>

`loom:verified-model-download`

</td>
<td>

Download an explicitly requested GGUF over HTTPS into Loom's private model library. Every request requires an expected SHA-256 digest and a hard byte ceiling.

</td>
</tr>

<tr>
<td>

`loom:microphone-capture`

</td>
<td>

Start, stop, or cancel a user-requested local microphone recording and its local speech-recognition request.

</td>
</tr>

<tr>
<td>

`loom:allow-application-close`

</td>
<td>

Enables the application_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-application-close`

</td>
<td>

Denies the application_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-application-close-abort`

</td>
<td>

Enables the application_close_abort command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-application-close-abort`

</td>
<td>

Denies the application_close_abort command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-application-close-pending`

</td>
<td>

Enables the application_close_pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-application-close-pending`

</td>
<td>

Denies the application_close_pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-attachment-import-batch-choose`

</td>
<td>

Enables the attachment_import_batch_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-attachment-import-batch-choose`

</td>
<td>

Denies the attachment_import_batch_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-attachment-import-choose`

</td>
<td>

Enables the attachment_import_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-attachment-import-choose`

</td>
<td>

Denies the attachment_import_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-attachment-import-paths`

</td>
<td>

Enables the attachment_import_paths command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-attachment-import-paths`

</td>
<td>

Denies the attachment_import_paths command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-attachment-ingest`

</td>
<td>

Enables the attachment_ingest command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-attachment-ingest`

</td>
<td>

Denies the attachment_ingest command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-attachment-reveal-original`

</td>
<td>

Enables the attachment_reveal_original command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-attachment-reveal-original`

</td>
<td>

Denies the attachment_reveal_original command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-audio-record-start`

</td>
<td>

Enables the audio_record_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-audio-record-start`

</td>
<td>

Denies the audio_record_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-audio-record-stop`

</td>
<td>

Enables the audio_record_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-audio-record-stop`

</td>
<td>

Denies the audio_record_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-audio-synthesize`

</td>
<td>

Enables the audio_synthesize command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-audio-synthesize`

</td>
<td>

Denies the audio_synthesize command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-branch-body`

</td>
<td>

Enables the branch_body command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-branch-body`

</td>
<td>

Denies the branch_body command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-branch-get`

</td>
<td>

Enables the branch_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-branch-get`

</td>
<td>

Denies the branch_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-branch-page`

</td>
<td>

Enables the branch_page command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-branch-page`

</td>
<td>

Denies the branch_page command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-build-model-policy-get`

</td>
<td>

Enables the build_model_policy_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-build-model-policy-get`

</td>
<td>

Denies the build_model_policy_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-edit`

</td>
<td>

Enables the cabal_edit command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-edit`

</td>
<td>

Denies the cabal_edit command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-join`

</td>
<td>

Enables the cabal_join command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-join`

</td>
<td>

Denies the cabal_join command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-network-get`

</td>
<td>

Enables the cabal_network_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-network-get`

</td>
<td>

Denies the cabal_network_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-network-set`

</td>
<td>

Enables the cabal_network_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-network-set`

</td>
<td>

Denies the cabal_network_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-open`

</td>
<td>

Enables the cabal_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-open`

</td>
<td>

Denies the cabal_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-recover`

</td>
<td>

Enables the cabal_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-recover`

</td>
<td>

Denies the cabal_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-revoke`

</td>
<td>

Enables the cabal_revoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-revoke`

</td>
<td>

Denies the cabal_revoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-share`

</td>
<td>

Enables the cabal_share command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-share`

</td>
<td>

Denies the cabal_share command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-snapshot`

</td>
<td>

Enables the cabal_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-snapshot`

</td>
<td>

Denies the cabal_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-cabal-workspace`

</td>
<td>

Enables the cabal_workspace command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-cabal-workspace`

</td>
<td>

Denies the cabal_workspace command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-candidate-keep`

</td>
<td>

Enables the candidate_keep command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-candidate-keep`

</td>
<td>

Denies the candidate_keep command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-candidate-promote`

</td>
<td>

Enables the candidate_promote command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-candidate-promote`

</td>
<td>

Denies the candidate_promote command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-co-writer-apply`

</td>
<td>

Enables the co_writer_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-co-writer-apply`

</td>
<td>

Denies the co_writer_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-co-writer-delete`

</td>
<td>

Enables the co_writer_delete command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-co-writer-delete`

</td>
<td>

Denies the co_writer_delete command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-co-writer-list`

</td>
<td>

Enables the co_writer_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-co-writer-list`

</td>
<td>

Denies the co_writer_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-co-writer-save`

</td>
<td>

Enables the co_writer_save command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-co-writer-save`

</td>
<td>

Denies the co_writer_save command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-completion-snapshot`

</td>
<td>

Enables the completion_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-completion-snapshot`

</td>
<td>

Denies the completion_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-grant`

</td>
<td>

Enables the compute_grant command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-grant`

</td>
<td>

Denies the compute_grant command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-host-snapshot`

</td>
<td>

Enables the compute_host_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-host-snapshot`

</td>
<td>

Denies the compute_host_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-job-cancel`

</td>
<td>

Enables the compute_job_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-job-cancel`

</td>
<td>

Denies the compute_job_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-job-check`

</td>
<td>

Enables the compute_job_check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-job-check`

</td>
<td>

Denies the compute_job_check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-job-get`

</td>
<td>

Enables the compute_job_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-job-get`

</td>
<td>

Denies the compute_job_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-job-prepare`

</td>
<td>

Enables the compute_job_prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-job-prepare`

</td>
<td>

Denies the compute_job_prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-job-submit`

</td>
<td>

Enables the compute_job_submit command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-job-submit`

</td>
<td>

Denies the compute_job_submit command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-jobs`

</td>
<td>

Enables the compute_jobs command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-jobs`

</td>
<td>

Denies the compute_jobs command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-peer-offers`

</td>
<td>

Enables the compute_peer_offers command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-peer-offers`

</td>
<td>

Denies the compute_peer_offers command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-compute-revoke`

</td>
<td>

Enables the compute_revoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-compute-revoke`

</td>
<td>

Denies the compute_revoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-checkpoint`

</td>
<td>

Enables the document_checkpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-checkpoint`

</td>
<td>

Denies the document_checkpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-add`

</td>
<td>

Enables the document_context_add command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-add`

</td>
<td>

Denies the document_context_add command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-add-many`

</td>
<td>

Enables the document_context_add_many command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-add-many`

</td>
<td>

Denies the document_context_add_many command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-list`

</td>
<td>

Enables the document_context_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-list`

</td>
<td>

Denies the document_context_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-remove`

</td>
<td>

Enables the document_context_remove command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-remove`

</td>
<td>

Denies the document_context_remove command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-snapshot-set`

</td>
<td>

Enables the document_context_snapshot_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-snapshot-set`

</td>
<td>

Denies the document_context_snapshot_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-text-get`

</td>
<td>

Enables the document_context_text_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-text-get`

</td>
<td>

Denies the document_context_text_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-context-text-set`

</td>
<td>

Enables the document_context_text_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-context-text-set`

</td>
<td>

Denies the document_context_text_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-create`

</td>
<td>

Enables the document_create command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-create`

</td>
<td>

Denies the document_create command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-delete`

</td>
<td>

Enables the document_delete command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-delete`

</td>
<td>

Denies the document_delete command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-draft-clear`

</td>
<td>

Enables the document_draft_clear command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-draft-clear`

</td>
<td>

Denies the document_draft_clear command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-draft-upsert`

</td>
<td>

Enables the document_draft_upsert command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-draft-upsert`

</td>
<td>

Denies the document_draft_upsert command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-export-choose`

</td>
<td>

Enables the document_export_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-export-choose`

</td>
<td>

Denies the document_export_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-import-external`

</td>
<td>

Enables the document_import_external command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-import-external`

</td>
<td>

Denies the document_import_external command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-open`

</td>
<td>

Enables the document_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-open`

</td>
<td>

Denies the document_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-reconcile-apply`

</td>
<td>

Enables the document_reconcile_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-reconcile-apply`

</td>
<td>

Denies the document_reconcile_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-reconciliation-preview`

</td>
<td>

Enables the document_reconciliation_preview command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-reconciliation-preview`

</td>
<td>

Denies the document_reconciliation_preview command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-rename`

</td>
<td>

Enables the document_rename command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-rename`

</td>
<td>

Denies the document_rename command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-document-reveal`

</td>
<td>

Enables the document_reveal command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-document-reveal`

</td>
<td>

Denies the document_reveal command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-focus-mode-set`

</td>
<td>

Enables the focus_mode_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-focus-mode-set`

</td>
<td>

Denies the focus_mode_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-generation-cancel`

</td>
<td>

Enables the generation_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-generation-cancel`

</td>
<td>

Denies the generation_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-account-cancel`

</td>
<td>

Enables the import_account_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-account-cancel`

</td>
<td>

Denies the import_account_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-account-connect`

</td>
<td>

Enables the import_account_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-account-connect`

</td>
<td>

Denies the import_account_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-account-disconnect`

</td>
<td>

Enables the import_account_disconnect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-account-disconnect`

</td>
<td>

Denies the import_account_disconnect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-account-sync`

</td>
<td>

Enables the import_account_sync command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-account-sync`

</td>
<td>

Denies the import_account_sync command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-accounts`

</td>
<td>

Enables the import_accounts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-accounts`

</td>
<td>

Denies the import_accounts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-source-url`

</td>
<td>

Enables the import_source_url command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-source-url`

</td>
<td>

Denies the import_source_url command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-import-text-sources`

</td>
<td>

Enables the import_text_sources command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-import-text-sources`

</td>
<td>

Denies the import_text_sources command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-catalog-list`

</td>
<td>

Enables the model_catalog_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-catalog-list`

</td>
<td>

Denies the model_catalog_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-choose`

</td>
<td>

Enables the model_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-choose`

</td>
<td>

Denies the model_choose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-download-cancel`

</td>
<td>

Enables the model_download_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-download-cancel`

</td>
<td>

Denies the model_download_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-download-list`

</td>
<td>

Enables the model_download_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-download-list`

</td>
<td>

Denies the model_download_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-download-start`

</td>
<td>

Enables the model_download_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-download-start`

</td>
<td>

Denies the model_download_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-download-status`

</td>
<td>

Enables the model_download_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-download-status`

</td>
<td>

Denies the model_download_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-list`

</td>
<td>

Enables the model_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-list`

</td>
<td>

Denies the model_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-load`

</td>
<td>

Enables the model_load command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-load`

</td>
<td>

Denies the model_load command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-load-catalog-candidate`

</td>
<td>

Enables the model_load_catalog_candidate command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-load-catalog-candidate`

</td>
<td>

Denies the model_load_catalog_candidate command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-load-policy-candidate`

</td>
<td>

Enables the model_load_policy_candidate command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-load-policy-candidate`

</td>
<td>

Denies the model_load_policy_candidate command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-model-unload`

</td>
<td>

Enables the model_unload command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-model-unload`

</td>
<td>

Denies the model_unload command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-close`

</td>
<td>

Enables the project_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-close`

</td>
<td>

Denies the project_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-commit-open`

</td>
<td>

Enables the project_commit_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-commit-open`

</td>
<td>

Denies the project_commit_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-current`

</td>
<td>

Enables the project_current command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-current`

</td>
<td>

Denies the project_current command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-discard-open`

</td>
<td>

Enables the project_discard_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-discard-open`

</td>
<td>

Denies the project_discard_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-drop-directories`

</td>
<td>

Enables the project_drop_directories command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-drop-directories`

</td>
<td>

Denies the project_drop_directories command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-open-default`

</td>
<td>

Enables the project_open_default command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-open-default`

</td>
<td>

Denies the project_open_default command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-prepare-open`

</td>
<td>

Enables the project_prepare_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-prepare-open`

</td>
<td>

Denies the project_prepare_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-prepare-open-path`

</td>
<td>

Enables the project_prepare_open_path command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-prepare-open-path`

</td>
<td>

Denies the project_prepare_open_path command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-recover`

</td>
<td>

Enables the project_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-recover`

</td>
<td>

Denies the project_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-shader-preview`

</td>
<td>

Enables the shader_preview command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-shader-preview`

</td>
<td>

Denies the shader_preview command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-signal-request`

</td>
<td>

Enables the signal_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-signal-request`

</td>
<td>

Denies the signal_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-cancel`

</td>
<td>

Enables the speech_input_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-cancel`

</td>
<td>

Denies the speech_input_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-capabilities`

</td>
<td>

Enables the speech_input_capabilities command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-capabilities`

</td>
<td>

Denies the speech_input_capabilities command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-record-cancel`

</td>
<td>

Enables the speech_input_record_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-record-cancel`

</td>
<td>

Denies the speech_input_record_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-record-start`

</td>
<td>

Enables the speech_input_record_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-record-start`

</td>
<td>

Denies the speech_input_record_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-record-stop`

</td>
<td>

Enables the speech_input_record_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-record-stop`

</td>
<td>

Denies the speech_input_record_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-speech-input-status`

</td>
<td>

Enables the speech_input_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-speech-input-status`

</td>
<td>

Denies the speech_input_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-suggestions-set`

</td>
<td>

Enables the suggestions_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-suggestions-set`

</td>
<td>

Denies the suggestions_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-terminal-cancel`

</td>
<td>

Enables the terminal_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-terminal-cancel`

</td>
<td>

Denies the terminal_cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-terminal-list`

</td>
<td>

Enables the terminal_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-terminal-list`

</td>
<td>

Denies the terminal_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-terminal-recover`

</td>
<td>

Enables the terminal_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-terminal-recover`

</td>
<td>

Denies the terminal_recover command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-terminal-run`

</td>
<td>

Enables the terminal_run command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-terminal-run`

</td>
<td>

Denies the terminal_run command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-terminal-run-peer`

</td>
<td>

Enables the terminal_run_peer command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-terminal-run-peer`

</td>
<td>

Denies the terminal_run_peer command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-weave-start`

</td>
<td>

Enables the weave_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-weave-start`

</td>
<td>

Denies the weave_start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-weave-status`

</td>
<td>

Enables the weave_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-weave-status`

</td>
<td>

Denies the weave_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-workspace-template-enable`

</td>
<td>

Enables the workspace_template_enable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-workspace-template-enable`

</td>
<td>

Denies the workspace_template_enable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-workspace-template-get`

</td>
<td>

Enables the workspace_template_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-workspace-template-get`

</td>
<td>

Denies the workspace_template_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:connected-import`

</td>
<td>

Explicit read-only account authorization and imports, with credentials in the OS credential store.

</td>
</tr>
</table>
