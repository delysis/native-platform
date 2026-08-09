## Default Permission

Open and edit a user-selected Loom project. Generation and hosted-provider authority are separate permissions.

#### This default permission set includes the following:

- `allow-project-open-default`
- `allow-project-choose-create`
- `allow-project-choose-open`
- `allow-project-close`
- `allow-project-current`
- `allow-project-recover`
- `allow-document-open`
- `allow-document-checkpoint`
- `allow-document-draft-upsert`
- `allow-document-draft-clear`
- `allow-document-reconciliation-preview`
- `allow-document-reconcile-apply`
- `allow-model-list`
- `allow-model-choose`
- `allow-model-download-status`
- `allow-model-download-list`
- `allow-branch-page`
- `allow-branch-get`
- `allow-branch-body`
- `allow-weave-status`
- `allow-application-close`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
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

`loom:allow-project-choose-create`

</td>
<td>

Enables the project_choose_create command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-choose-create`

</td>
<td>

Denies the project_choose_create command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:allow-project-choose-open`

</td>
<td>

Enables the project_choose_open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`loom:deny-project-choose-open`

</td>
<td>

Denies the project_choose_open command without any pre-configured scope.

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
</table>
