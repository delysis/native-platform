fn resolve_targets(handles: &[String], host_id: &str) -> Result<TargetResolution> {
    let (conversations, groups) = conversation_and_group_handles()?;
    Ok(resolve_targets_from_registry(
        handles,
        host_id,
        &conversations,
        &groups,
    ))
}

include!("mentions/target_resolution.rs");

fn resolve_targets_from_registry(
    handles: &[String],
    host_id: &str,
    conversations: &[Conversation],
    groups: &[crate::personas::PersonaGroup],
) -> TargetResolution {
    checked_resolve_targets_from_registry(handles, host_id, conversations, groups)
}

fn snapshot_target(target: &ResolvedTarget) -> Result<MentionTargetSnapshot> {
