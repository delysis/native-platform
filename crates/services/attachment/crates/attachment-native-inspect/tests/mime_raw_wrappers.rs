use attachment_native_inspect::{Inspector, ProvidedAttachment};
use attachment_native_types::{
    AttachmentBundle, BudgetLimits, Coverage, EdgeOutcome, InspectionPolicy, IssueClass,
    IssueSeverity, ObjectStatus,
};
use mail_parser::{MessageParser, PartType};

fn raw_chain(subtype: &str, wrappers: usize) -> Vec<u8> {
    let mut message = "From: leaf@example.test\r\nSubject: Leaf\r\nContent-Type: text/plain\r\n\r\nkept evidence\r\n".to_string();
    for index in (0..wrappers).rev() {
        message = format!(
            "From: wrapper-{index}@example.test\r\nSubject: Wrapper {index}\r\nContent-Type: message/{subtype}\r\nContent-Disposition: attachment; filename=inner-{index}.eml\r\n\r\n{message}"
        );
    }
    message.into_bytes()
}

fn inspect(bytes: &[u8], limits: BudgetLimits) -> AttachmentBundle {
    let bundle = Inspector::new(InspectionPolicy {
        limits,
        ..InspectionPolicy::default()
    })
    .expect("valid policy")
    .inspect(ProvidedAttachment::from_bytes(
        "raw-witness.eml",
        Some("message/rfc822".into()),
        bytes,
    ))
    .expect("public inspection");
    bundle.validate().expect("graph accounting");
    assert_eq!(
        bundle
            .blobs
            .get(&bundle.graph.root)
            .expect("original root")
            .as_ref(),
        bytes
    );
    bundle
}

fn assert_early_refusal(bundle: &AttachmentBundle, code: &str) {
    assert!(matches!(bundle.graph.coverage, Coverage::Partial { .. }));
    assert_eq!(bundle.graph.objects.len(), 1);
    assert!(bundle.graph.edges.is_empty());
    assert_eq!(bundle.graph.usage.entries, 0);
    assert_eq!(bundle.graph.usage.total_derived_bytes, 0);
    assert!(matches!(
        &bundle.graph.objects[0].status,
        ObjectStatus::Partial { reasons } if reasons.iter().any(|reason| reason == code)
    ));
    assert!(
        bundle.graph.issues.iter().any(|issue| {
            issue.code == code
                && issue.class == IssueClass::Budget
                && issue.severity == IssueSeverity::Blocked
        }),
        "{:#?}",
        bundle.graph
    );
}

#[test]
fn raw_wrappers_respect_each_remaining_graph_allowance_before_materialization() {
    for subtype in ["rfc822", "global"] {
        let bytes = raw_chain(subtype, 2);
        for limits in [
            BudgetLimits {
                max_entries: 1,
                ..BudgetLimits::default()
            },
            BudgetLimits {
                max_edges: 1,
                ..BudgetLimits::default()
            },
            BudgetLimits {
                max_objects: 2,
                ..BudgetLimits::default()
            },
        ] {
            assert_early_refusal(&inspect(&bytes, limits), "mime_part_limit_exceeded");
        }
    }
}

#[test]
fn raw_wrappers_keep_exact_part_depth_and_public_contents_controls() {
    for subtype in ["rfc822", "global"] {
        let bytes = raw_chain(subtype, 2);
        let parsed = MessageParser::default()
            .parse(&bytes)
            .expect("current parser");
        assert_eq!(parsed.attachment_count(), 1);
        let part = parsed.attachments().next().expect("outer wrapper");
        let PartType::Message(inner) = &part.body else {
            panic!("raw nested Message")
        };
        assert_eq!(inner.attachment_count(), 1);
        let inner_part = inner.attachments().next().expect("inner wrapper");
        let PartType::Message(leaf) = &inner_part.body else {
            panic!("raw leaf Message")
        };
        assert_eq!(leaf.attachment_count(), 0);
        let body_offset = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("fixture separator")
            + 4;
        assert_eq!(part.contents(), &bytes[body_offset..]);
        assert_eq!(part.contents(), inner.raw_message());
        let inner_body_offset = part
            .contents()
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("inner separator")
            + 4;
        assert_eq!(inner_part.contents(), &part.contents()[inner_body_offset..]);
        assert_eq!(inner_part.contents(), leaf.raw_message());

        let bundle = inspect(
            &bytes,
            BudgetLimits {
                max_entries: 2,
                max_edges: 2,
                max_objects: 3,
                max_depth: 2,
                ..BudgetLimits::default()
            },
        );
        assert_eq!(bundle.graph.coverage, Coverage::Complete);
        assert_eq!(bundle.graph.usage.entries, 2);
        assert_eq!(bundle.graph.objects.len(), 3);
        assert_eq!(bundle.graph.edges.len(), 2);
        assert!(
            bundle
                .graph
                .edges
                .iter()
                .all(|edge| edge.outcome == EdgeOutcome::Derived)
        );
        assert!(bundle.graph.issues.is_empty(), "{:#?}", bundle.graph);
    }
}

#[test]
fn raw_wrapper_depth_is_admitted_before_eager_descent() {
    for subtype in ["rfc822", "global"] {
        assert_early_refusal(
            &inspect(
                &raw_chain(subtype, 2),
                BudgetLimits {
                    max_depth: 1,
                    ..BudgetLimits::default()
                },
            ),
            "mime_message_depth_exceeded",
        );
    }
}

#[test]
fn raw_multipart_and_digest_siblings_restore_depth_without_double_charging() {
    for subtype in ["mixed", "digest"] {
        let mut message = format!("From: source@example.test\r\nSubject: Siblings\r\nContent-Type: multipart/{subtype}; boundary=outer\r\n\r\n");
        for index in 0..2 {
            let content_type = if subtype == "mixed" {
                "Content-Type: message/rfc822\r\n"
            } else {
                ""
            };
            message.push_str(&format!("--outer\r\n{content_type}Content-Disposition: attachment; filename=sibling-{index}.eml\r\n\r\nFrom: leaf-{index}@example.test\r\nSubject: Leaf {index}\r\nContent-Type: text/plain\r\n\r\nkept {index}\r\n"));
        }
        message.push_str("--outer--\r\n");
        let bundle = inspect(
            message.as_bytes(),
            BudgetLimits {
                max_entries: 2,
                max_edges: 2,
                max_objects: 3,
                max_depth: 1,
                ..BudgetLimits::default()
            },
        );
        assert_eq!(bundle.graph.coverage, Coverage::Complete);
        assert_eq!(bundle.graph.usage.entries, 2);
        assert_eq!(bundle.graph.objects.len(), 3);
        assert_eq!(bundle.graph.edges.len(), 2);
        assert!(bundle.graph.edges.iter().all(|edge| {
            edge.depth == 1 && edge.outcome == EdgeOutcome::Derived
        }));
        assert!(bundle.graph.issues.is_empty(), "{:#?}", bundle.graph);
    }
}

#[test]
fn missing_empty_and_unmatched_boundaries_charge_the_recovered_attachment() {
    for content_type in [
        "multipart/mixed",
        "multipart/mixed; boundary=\"\"",
        "multipart/mixed; boundary=absent",
    ] {
        let bytes = format!("From: source@example.test\r\nSubject: Recovery\r\nContent-Type: {content_type}\r\n\r\nrecovered evidence\r\n");
        assert_eq!(
            MessageParser::default()
                .parse(bytes.as_bytes())
                .expect("current recovery")
                .attachment_count(),
            1
        );
        assert_early_refusal(
            &inspect(
                bytes.as_bytes(),
                BudgetLimits {
                    max_objects: 1,
                    ..BudgetLimits::default()
                },
            ),
            "mime_part_limit_exceeded",
        );
    }
}

#[test]
fn ordinary_inline_and_header_only_mail_need_no_child_allowance() {
    for bytes in [
        "From: source@example.test\r\nSubject: Inline\r\n\r\ninline evidence\r\n",
        "From: source@example.test\r\nSubject: Headers only\r\n\r\n",
    ] {
        let bundle = inspect(
            bytes.as_bytes(),
            BudgetLimits {
                max_objects: 1,
                ..BudgetLimits::default()
            },
        );
        assert_eq!(bundle.graph.coverage, Coverage::Complete);
        assert_eq!(bundle.graph.objects.len(), 1);
        assert!(bundle.graph.edges.is_empty());
        assert_eq!(bundle.graph.usage.entries, 0);
        assert!(bundle.graph.issues.is_empty(), "{:#?}", bundle.graph);
    }
}
