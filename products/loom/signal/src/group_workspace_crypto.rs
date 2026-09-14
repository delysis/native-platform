//! Authenticate the exact description accepted by the group service. The
//! worker owns this check; Presage supplies transport, not publication authority.
use presage::libsignal_service::{
    self,
    prelude::{GroupSecretParams, ProtobufMessage},
};
// Validate the signed server response independently of the network. In
// particular, decrypt_group_change() deliberately does not check its signature.
pub(super) fn verify_description_change(
    params: GroupSecretParams,
    server: libsignal_service::zkgroup::ServerPublicParams,
    editor: libsignal_service::protocol::Aci,
    mut expected: libsignal_service::proto::group_change::Actions,
    change: &libsignal_service::proto::GroupChange,
) -> Result<(), libsignal_service::prelude::ServiceError> {
    use libsignal_service::{
        groups_v2::GroupOperations, prelude::ServiceError, proto::group_change,
    };
    let invalid = || ServiceError::InvalidFrame {
        reason: "unrelated group change",
    };
    server
        .verify_signature(
            &change.actions,
            change
                .server_signature
                .as_slice()
                .try_into()
                .map_err(|_| invalid())?,
        )
        .map_err(ServiceError::from)?;
    let actual =
        group_change::Actions::decode(change.actions.as_slice()).map_err(ServiceError::from)?;
    expected.group_id = params.get_public_params().get_group_identifier().to_vec();
    expected.source_user_id = actual.source_user_id.clone();
    let decoded = GroupOperations::new(params)
        .decrypt_group_change(change.clone())
        .map_err(ServiceError::from)?;
    if actual != expected || decoded.editor != editor {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod description_tests {
    use super::*;
    use presage::libsignal_service::{
        groups_v2::GroupOperations,
        proto::{GroupChange, group_change},
        zkgroup::{self, ServerSecretParams},
    };
    use presage::libsignal_service::{
        prelude::{GroupMasterKey, Uuid},
        protocol::Aci,
    };

    #[test]
    fn description_response_requires_the_signature_exact_group_editor_revision_and_attributes() {
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new([7; 32]));
        let server = ServerSecretParams::generate([8; 32]);
        let editor = Aci::from(Uuid::from_bytes([9; 16]));
        let expected = group_change::Actions {
            version: 12,
            modify_description: Some(group_change::actions::ModifyDescriptionAction {
                description: GroupOperations::new(params)
                    .encrypt_description(Some("Our workspace"), &mut rand::rng()),
            }),
            ..Default::default()
        };
        let mut actual = expected.clone();
        actual.group_id = params.get_public_params().get_group_identifier().to_vec();
        actual.source_user_id = zkgroup::serialize(&params.encrypt_service_id(editor.into()));
        let sign = |actions: &group_change::Actions| {
            let actions = actions.encode_to_vec();
            let server_signature = server.sign([10; 32], &actions).to_vec();
            GroupChange {
                actions,
                server_signature,
                change_epoch: 5,
            }
        };
        let validate = |change: &GroupChange| {
            verify_description_change(
                params,
                server.get_public_params(),
                editor,
                expected.clone(),
                change,
            )
        };
        assert!(validate(&sign(&actual)).is_ok());
        let mut corrupt = sign(&actual);
        corrupt.server_signature[0] ^= 1;
        assert!(validate(&corrupt).is_err());
        let mut wrong = actual.clone();
        wrong.version += 1;
        assert!(validate(&sign(&wrong)).is_err());
        let mut wrong = actual.clone();
        wrong.group_id[0] ^= 1;
        assert!(validate(&sign(&wrong)).is_err());
        let mut wrong = actual.clone();
        wrong.source_user_id = zkgroup::serialize(
            &params.encrypt_service_id(Aci::from(Uuid::from_bytes([11; 16])).into()),
        );
        assert!(validate(&sign(&wrong)).is_err());
        let mut wrong = actual.clone();
        wrong.modify_description.as_mut().unwrap().description = GroupOperations::new(params)
            .encrypt_description(Some("Unreviewed text"), &mut rand::rng());
        assert!(validate(&sign(&wrong)).is_err());
        let mut wrong = actual.clone();
        wrong.modify_title = Some(group_change::actions::ModifyTitleAction { title: vec![] });
        assert!(validate(&sign(&wrong)).is_err());
    }
}
